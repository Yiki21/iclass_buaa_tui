//! Read-only academic services used by the course-focused TUI and CLI.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::constants::{
    BUAA_CLASSROOM_QUERY_URL, BUAA_CLASSROOM_REFERRER, BUAA_CLASSROOM_SYNC_URL, BUAA_SCORE_URL,
    BYXT_EXAMS_URL, to_webvpn_url,
};
use crate::iclass::IClassApi;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct ExamItem {
    pub course_name:      String,
    pub course_no:        Option<String>,
    pub exam_date:        Option<String>,
    pub start_time:       Option<String>,
    pub end_time:         Option<String>,
    pub time_description: Option<String>,
    pub place:            Option<String>,
    pub seat:             Option<String>,
    pub exam_type:        Option<String>,
    pub status:           Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]

pub struct GradeItem {
    pub id:          Option<String>,
    pub term_code:   String,
    pub course_name: String,
    pub course_code: Option<String>,
    pub credit:      Option<f64>,
    pub score:       Option<String>,
    pub grade_point: Option<String>,
    pub passed:      Option<String>,
    pub exam_type:   Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct ClassroomRoom {
    pub building:      String,
    pub name:          String,
    pub floor_id:      String,
    pub free_sections: Vec<usize>,
}

impl IClassApi {
    pub async fn get_exams(&self, term_code: &str) -> Result<Vec<ExamItem>> {

        let response = self
            .client
            .get(academic_url(self.use_vpn, BYXT_EXAMS_URL))
            .query(&[("termCode", term_code)])
            .header("Accept", "application/json, text/javascript, */*; q=0.01")
            .header("X-Requested-With", "XMLHttpRequest")
            .send()
            .await
            .context("请求考试安排失败")?;

        let status = response.status().as_u16();

        let final_url = response.url().to_string();

        let body = response.text().await.context("读取考试安排失败")?;

        ensure_academic_response(status, &final_url, &body, "考试安排")?;

        let root: Value = serde_json::from_str(&body).context("考试安排响应格式异常")?;

        let code = scalar_text(root.get("code")).unwrap_or_default();

        if code != "0" {

            bail!(
                "考试安排查询失败: {}",
                scalar_text(root.get("msg")).unwrap_or_default()
            );
        }

        Ok(root
            .get("datas")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(parse_exam)
            .collect())
    }

    pub async fn get_grades(&self, term_code: &str) -> Result<Vec<GradeItem>> {

        let (year, semester) = parse_grade_term(term_code)?;

        let score_url = academic_url(self.use_vpn, BUAA_SCORE_URL);

        let activation = self
            .client
            .get(&score_url)
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .send()
            .await
            .context("打开成绩查询页面失败")?;

        let activation_status = activation.status().as_u16();

        let activation_url = activation.url().to_string();

        let activation_body = activation.text().await.context("读取成绩查询页面失败")?;

        ensure_academic_response(
            activation_status,
            &activation_url,
            &activation_body,
            "成绩查询",
        )?;

        let response = self
            .client
            .post(&score_url)
            .header("Accept", "application/json, text/javascript, */*; q=0.01")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", &score_url)
            .form(&[("xq", semester.as_str()), ("year", year.as_str())])
            .send()
            .await
            .context("请求成绩列表失败")?;

        let status = response.status().as_u16();

        let final_url = response.url().to_string();

        let body = response.text().await.context("读取成绩列表失败")?;

        ensure_academic_response(status, &final_url, &body, "成绩查询")?;

        let root: Value = serde_json::from_str(&body).context("成绩响应格式异常")?;

        if scalar_text(root.get("e")).as_deref() != Some("0") {

            bail!(
                "成绩查询失败: {}",
                scalar_text(root.get("m")).unwrap_or_default()
            );
        }

        let data = root
            .get("d")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        Ok(data
            .into_values()
            .filter_map(|row| row.as_object().map(|object| parse_grade(object, term_code)))
            .collect())
    }

    pub async fn query_classrooms(&self, campus: i64, date: &str) -> Result<Vec<ClassroomRoom>> {

        let sync_url = academic_url(self.use_vpn, BUAA_CLASSROOM_SYNC_URL);

        let _ = self
            .client
            .get(sync_url)
            .header("User-Agent", classroom_user_agent())
            .send()
            .await
            .context("同步空教室会话失败")?;

        let response = self
            .client
            .get(academic_url(self.use_vpn, BUAA_CLASSROOM_QUERY_URL))
            .query(&[
                ("xqid", campus.to_string()),
                ("floorid", String::new()),
                ("date", date.to_string()),
            ])
            .header("User-Agent", classroom_user_agent())
            .header("Accept", "application/json, text/javascript, */*; q=0.01")
            .header("X-Requested-With", "XMLHttpRequest")
            .header(
                "Referer",
                academic_url(self.use_vpn, BUAA_CLASSROOM_REFERRER),
            )
            .send()
            .await
            .context("请求空教室列表失败")?;

        let status = response.status().as_u16();

        let final_url = response.url().to_string();

        let body = response.text().await.context("读取空教室列表失败")?;

        ensure_academic_response(status, &final_url, &body, "空教室查询")?;

        let root: Value = serde_json::from_str(&body).context("空教室响应格式异常")?;

        let data = root
            .get("d")
            .and_then(|value| value.get("list"))
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("空教室响应缺少教室列表"))?;

        let mut rooms = data
            .iter()
            .flat_map(|(building, values)| {

                values
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(move |room| parse_classroom(building, room))
            })
            .collect::<Vec<_>>();

        rooms.sort_by(|left, right| left.name.cmp(&right.name));

        Ok(rooms)
    }
}

fn academic_url(use_vpn: bool, raw: &str) -> String {

    if use_vpn {

        to_webvpn_url(raw)
    } else {

        raw.to_string()
    }
}

fn ensure_academic_response(status: u16, final_url: &str, body: &str, stage: &str) -> Result<()> {

    let lower = body.to_ascii_lowercase();

    if final_url
        .to_ascii_lowercase()
        .contains("sso.buaa.edu.cn/login")
        || lower.contains("name=\"execution\"")
        || lower.contains("统一身份认证")
    {

        bail!("{stage}失败：统一认证会话已失效，请重新登录");
    }

    if !(200..300).contains(&status) {

        bail!("{stage}失败：HTTP {status}");
    }

    Ok(())
}

fn parse_grade_term(term_code: &str) -> Result<(String, String)> {

    let (year, semester) = term_code
        .rsplit_once('-')
        .ok_or_else(|| anyhow!("成绩查询需要形如 2025-2026-1 的学期代码"))?;

    if year.len() != 9 || semester.parse::<u8>().is_err() {

        bail!("成绩查询需要形如 2025-2026-1 的学期代码");
    }

    Ok((year.to_string(), semester.to_string()))
}

fn parse_exam(row: &Value) -> ExamItem {

    ExamItem {
        course_name:      string_field(row, &["courseName", "kcmc"])
            .unwrap_or_else(|| "未命名课程".to_string()),
        course_no:        string_field(row, &["courseNo", "kch"]),
        exam_date:        string_field(row, &["examDate", "ksrq"]),
        start_time:       string_field(row, &["startTime", "kssj"]),
        end_time:         string_field(row, &["endTime", "jssj"]),
        time_description: string_field(row, &["examTimeDescription", "kssjms"]),
        place:            string_field(row, &["examPlace", "kcd"]),
        seat:             string_field(row, &["examSeatNo", "zwh"]),
        exam_type:        string_field(row, &["examType", "kslx"]),
        status:           row.get("examStatus").and_then(Value::as_i64),
    }
}

fn parse_grade(row: &Map<String, Value>, term_code: &str) -> GradeItem {

    GradeItem {
        id:          string_field_map(row, &["kch", "id"]),
        term_code:   term_code.to_string(),
        course_name: string_field_map(row, &["kcmc", "courseName"])
            .unwrap_or_else(|| "未命名课程".to_string()),
        course_code: string_field_map(row, &["kch", "courseCode"]),
        credit:      row.get("xf").and_then(number_value),
        score:       string_field_map(row, &["kccj", "score"]),
        grade_point: string_field_map(row, &["jd", "gradePoint"]),
        passed:      string_field_map(row, &["sfjg", "passed"]),
        exam_type:   string_field_map(row, &["fslx", "examType"]),
    }
}

fn parse_classroom(building: &str, row: &Value) -> Option<ClassroomRoom> {

    Some(ClassroomRoom {
        building:      building.to_string(),
        name:          string_field(row, &["name", "jasmc"])?,
        floor_id:      string_field(row, &["floorid", "floorId"]).unwrap_or_default(),
        free_sections: string_field(row, &["kxsds", "freeSections"])
            .unwrap_or_default()
            .split(',')
            .filter_map(|value| value.trim().parse().ok())
            .collect(),
    })
}

fn string_field(row: &Value, keys: &[&str]) -> Option<String> {

    keys.iter()
        .find_map(|key| row.get(*key).and_then(scalar_value_text))
}

fn string_field_map(row: &Map<String, Value>, keys: &[&str]) -> Option<String> {

    keys.iter()
        .find_map(|key| row.get(*key).and_then(scalar_value_text))
}

fn scalar_text(value: Option<&Value>) -> Option<String> {

    value.and_then(scalar_value_text)
}

fn scalar_value_text(value: &Value) -> Option<String> {

    match value {
        Value::String(value) => Some(value.trim().to_string()).filter(|value| !value.is_empty()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn number_value(value: &Value) -> Option<f64> {

    match value {
        Value::Number(value) => value.as_f64(),
        Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}

fn classroom_user_agent() -> &'static str {

    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/138 Safari/537.36"
}

#[cfg(test)]

mod tests {

    use super::{parse_classroom, parse_exam, parse_grade_term};
    use serde_json::json;

    #[test]

    fn parses_academic_term_for_score_endpoint() {

        assert_eq!(
            parse_grade_term("2025-2026-1").unwrap(),
            ("2025-2026".into(), "1".into())
        );

        assert!(parse_grade_term("20261").is_err());
    }

    #[test]

    fn parses_exam_and_classroom_rows() {

        let exam = parse_exam(
            &json!({"courseName":"高等数学","examDate":"2026-01-10","examPlace":"主M101"}),
        );

        assert_eq!(exam.course_name, "高等数学");

        assert_eq!(exam.place.as_deref(), Some("主M101"));

        let room = parse_classroom(
            "学院路",
            &json!({"name":"J1-101","floorid":"1","kxsds":"1,2,14"}),
        )
        .unwrap();

        assert_eq!(room.free_sections, vec![1, 2, 14]);
    }
}
