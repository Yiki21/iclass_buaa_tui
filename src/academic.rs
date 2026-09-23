//! Read-only academic services used by the course-focused TUI and CLI.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::constants::{
    BUAA_CLASSROOM_QUERY_URL, BUAA_CLASSROOM_REFERRER, BUAA_CLASSROOM_SYNC_URL, BUAA_SCORE_URL,
    BYXT_EXAMS_URL, BYXT_ROOT_URL, to_webvpn_url,
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

/// Result of loading several terms at once.

#[derive(Clone, Debug, Default, PartialEq)]

pub struct GradesForTerms {
    pub grades: Vec<GradeItem>,
    /// Terms that could not be loaded, as `(term_code, error)`.
    pub failed: Vec<(String, String)>,
}

/// Credit-weighted summary over a set of grades.
///
/// Why:
/// The list of individual scores is what the portal shows, but the number a
/// student actually wants is the GPA. Computing it here, next to the parser,
/// keeps one definition of "which rows count" shared by the TUI and the CLI.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]

pub struct GradeSummary {
    /// Rows that contributed to the averages: those with a credit and a
    /// numeric grade point.
    pub counted:        usize,
    /// Rows skipped for lacking a credit or a numeric grade point (pass/fail,
    /// pending, exempt).
    pub skipped:        usize,
    pub total_credits:  f64,
    pub weighted_gpa:   Option<f64>,
    pub weighted_score: Option<f64>,
    pub failed:         usize,
}

/// Computes a credit-weighted GPA and average score.
///
/// How:
/// A row contributes when it has a credit and a numeric grade point. The
/// weighted score is computed independently and only over rows that also have
/// a numeric score, so a pass/fail course with a grade point but no percentage
/// still counts toward GPA. Failed is counted from the score when present,
/// otherwise from the portal's own pass flag.

pub fn summarize_grades(grades: &[GradeItem]) -> GradeSummary {

    let mut summary = GradeSummary::default();

    let mut gpa_weight = 0.0_f64;

    let mut gpa_sum = 0.0_f64;

    let mut score_weight = 0.0_f64;

    let mut score_sum = 0.0_f64;

    for grade in grades {

        let credit = grade.credit.filter(|value| *value > 0.0);

        let point = grade
            .grade_point
            .as_deref()
            .and_then(|value| value.trim().parse::<f64>().ok());

        let score = grade
            .score
            .as_deref()
            .and_then(|value| value.trim().parse::<f64>().ok());

        let Some(credit) = credit else {

            summary.skipped += 1;

            continue;
        };

        let Some(point) = point else {

            summary.skipped += 1;

            continue;
        };

        summary.counted += 1;

        summary.total_credits += credit;

        gpa_weight += credit;

        gpa_sum += credit * point;

        if let Some(score) = score {

            score_weight += credit;

            score_sum += credit * score;

            if score < 60.0 {

                summary.failed += 1;
            }
        } else if grade
            .passed
            .as_deref()
            .is_some_and(|flag| flag.contains("不通过") || flag.contains("未通过"))
        {

            summary.failed += 1;
        }
    }

    if gpa_weight > 0.0 {

        summary.weighted_gpa = Some(gpa_sum / gpa_weight);
    }

    if score_weight > 0.0 {

        summary.weighted_score = Some(score_sum / score_weight);
    }

    summary
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct ClassroomRoom {
    pub building:      String,
    pub name:          String,
    pub floor_id:      String,
    pub free_sections: Vec<usize>,
}

impl IClassApi {
    /// Visits the BYXT portal root so it issues its session cookie.
    ///
    /// Why:
    /// Every BYXT endpoint answers 401 until the portal itself has been
    /// visited through the SSO chain. Failures are ignored on purpose: this
    /// runs before a call that reports the real problem, so a portal that is
    /// genuinely unreachable should surface there rather than as an activation
    /// error.

    pub(crate) async fn activate_byxt_portal(&self) {

        let _ = self
            .client
            .get(academic_url(self.use_vpn, BYXT_ROOT_URL))
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .send()
            .await;
    }

    pub async fn get_exams(&self, term_code: &str) -> Result<Vec<ExamItem>> {

        // Establish the BYXT session before querying it.
        //
        // Why:
        // BYXT issues its own session cookie only once it has been visited
        // through the SSO chain; the exam endpoint answers a bare 401 until
        // then. The grades path already did this, which is why grades worked
        // while exams did not.
        //
        // The entry must be the portal root, not `/jwapp/sys/homeapp/index.html`:
        // that page answers 404 when requested directly and is only valid as a
        // Referer.
        self.activate_byxt_portal().await;

        let response = self
            .client
            .get(academic_url(self.use_vpn, BYXT_EXAMS_URL))
            .query(&[("termCode", term_code)])
            .header("Accept", "application/json, text/javascript, */*; q=0.01")
            .header("X-Requested-With", "XMLHttpRequest")
            // The exam endpoint is served from the home sub-app, and upstream
            // sends that page as the referrer rather than the portal root.
            .header(
                "Referer",
                academic_url(
                    self.use_vpn,
                    "https://byxt.buaa.edu.cn/jwapp/sys/homeapp/home/index.html",
                ),
            )
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

    /// Fetches grades for several terms and flattens them into one list.
    ///
    /// Why:
    /// Grades are per-term upstream, but a student looking at their record
    /// wants the whole thing at once. Loading term by term also means one
    /// failing term should not hide the others.
    ///
    /// How:
    /// Terms run concurrently, bounded so the portal is not flooded. A term
    /// that fails is reported by code and skipped; the rest still return.

    pub async fn get_grades_for_terms(&self, term_codes: &[String]) -> GradesForTerms {

        use std::sync::Arc;
        use tokio::sync::Semaphore;

        const CONCURRENCY: usize = 4;

        let semaphore = Arc::new(Semaphore::new(CONCURRENCY));

        let mut tasks = Vec::with_capacity(term_codes.len());

        for term_code in term_codes {

            // The semaphore is never closed, so this only fails if it were,
            // which would mean the whole batch is over anyway.
            let Ok(permit) = semaphore.clone().acquire_owned().await else {

                break;
            };

            let api = self.clone();

            let term = term_code.clone();

            tasks.push(tokio::spawn(async move {

                let _permit = permit;

                let outcome = api.get_grades(&term).await;

                (term, outcome)
            }));
        }

        let mut grades = Vec::new();

        let mut failed = Vec::new();

        for task in tasks {

            let (term_code, outcome) = match task.await {
                Ok(value) => value,
                Err(error) => {

                    failed.push((String::from("<unknown>"), error.to_string()));

                    continue;
                }
            };

            match outcome {
                Ok(mut items) => grades.append(&mut items),
                Err(error) => failed.push((term_code, error.to_string())),
            }
        }

        // Newest term first, then stable within a term by course name.
        grades.sort_by(|left, right| {

            right
                .term_code
                .cmp(&left.term_code)
                .then_with(|| left.course_name.cmp(&right.course_name))
        });

        failed.sort();

        GradesForTerms { grades, failed }
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

        // A non-2xx body often names the actual problem (unknown endpoint, bad
        // parameter, wrong referrer) while the status alone says nothing.
        if std::env::var_os("ICLASS_ACADEMIC_DEBUG").is_some() {

            let preview: String = body.chars().take(300).collect();

            eprintln!(
                "{stage} HTTP {status} @ {final_url} body: {}",
                preview.replace('\n', " ")
            );
        }

        bail!("{stage}失败：HTTP {status}");
    }

    Ok(())
}

/// Whether a term code has the shape the grade endpoint accepts.
///
/// Why:
/// Graduate GSMIS terms are five-digit codes like `20261`; the score portal
/// only understands `2025-2026-1`. Filtering up front keeps the all-terms load
/// from firing a doomed request per graduate term.

pub fn looks_like_score_term(term_code: &str) -> bool {

    parse_grade_term(term_code).is_ok()
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

    use super::{
        GradeItem, looks_like_score_term, parse_classroom, parse_exam, parse_grade_term,
        summarize_grades,
    };
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

    fn grade(credit: Option<f64>, score: Option<&str>, point: Option<&str>) -> GradeItem {

        GradeItem {
            credit,
            score: score.map(str::to_string),
            grade_point: point.map(str::to_string),
            ..GradeItem::default()
        }
    }

    #[test]

    fn summary_weights_gpa_by_credit() {

        // 4 credits at 4.0 and 2 credits at 2.0 -> (16 + 4) / 6 = 3.333...
        let grades = vec![
            grade(Some(4.0), Some("95"), Some("4.0")),
            grade(Some(2.0), Some("70"), Some("2.0")),
        ];

        let summary = summarize_grades(&grades);

        assert_eq!(summary.counted, 2);

        assert_eq!(summary.total_credits, 6.0);

        let gpa = summary.weighted_gpa.expect("应有 GPA");

        assert!((gpa - 3.3333).abs() < 0.001, "GPA 应按学分加权，得到 {gpa}");

        let avg = summary.weighted_score.expect("应有加权分");

        // (4*95 + 2*70) / 6 = 520 / 6 = 86.67
        assert!(
            (avg - 86.6667).abs() < 0.001,
            "加权分应为 86.67，得到 {avg}"
        );
    }

    #[test]

    fn summary_skips_rows_without_credit_or_numeric_point() {

        let grades = vec![
            grade(Some(3.0), Some("88"), Some("3.7")),
            // Pass/fail: has credit but no numeric grade point.
            grade(Some(1.0), Some("合格"), None),
            // Pending: no credit.
            grade(None, Some("90"), Some("4.0")),
            // Zero-credit placeholder.
            grade(Some(0.0), Some("100"), Some("4.0")),
        ];

        let summary = summarize_grades(&grades);

        assert_eq!(summary.counted, 1, "只有第一条应计入");

        assert_eq!(summary.skipped, 3);

        assert_eq!(summary.total_credits, 3.0);
    }

    #[test]

    fn summary_counts_failures_from_score_or_pass_flag() {

        let mut flagged = grade(Some(2.0), None, Some("0"));

        flagged.passed = Some("不通过".to_string());

        let grades = vec![
            grade(Some(3.0), Some("55"), Some("0")),
            flagged,
            grade(Some(2.0), Some("75"), Some("2.5")),
        ];

        let summary = summarize_grades(&grades);

        assert_eq!(summary.failed, 2, "一条按分数、一条按通过标记判为不及格");
    }

    #[test]

    fn summary_ignores_terms_without_a_numeric_scale() {

        // A full term of realistic data: two strong, one weak, one pass/fail.
        let grades = vec![
            grade(Some(4.0), Some("92"), Some("3.9")),
            grade(Some(3.0), Some("85"), Some("3.5")),
            grade(Some(2.0), Some("58"), Some("0.0")),
            grade(Some(1.0), Some("合格"), None),
        ];

        let summary = summarize_grades(&grades);

        assert_eq!(summary.counted, 3, "合格/不合格不应计入 GPA");

        assert_eq!(summary.skipped, 1);

        assert_eq!(summary.failed, 1);

        assert_eq!(summary.total_credits, 9.0);

        let gpa = summary.weighted_gpa.expect("应有 GPA");

        // (4*3.9 + 3*3.5 + 2*0.0) / 9 = 26.1 / 9 = 2.9
        assert!((gpa - 2.9).abs() < 0.0001, "GPA 应为 2.9，得到 {gpa}");
    }

    #[test]

    fn score_term_shape_is_checked_before_requesting() {

        // Undergraduate codes go through; graduate five-digit codes do not.
        assert!(looks_like_score_term("2025-2026-1"));

        assert!(looks_like_score_term("2024-2025-2"));

        assert!(!looks_like_score_term("20261"));

        assert!(!looks_like_score_term("2025-2026"));

        assert!(!looks_like_score_term(""));
    }

    #[test]

    fn summary_of_nothing_has_no_averages() {

        let summary = summarize_grades(&[]);

        assert_eq!(summary.weighted_gpa, None);

        assert_eq!(summary.weighted_score, None);

        assert_eq!(summary.counted, 0);
    }
}
