//! Read-only assignment aggregation for the terminal course workspace.

use aes::{
    Aes128,
    cipher::{Array, BlockCipherEncrypt, KeyInit},
};
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::constants::to_webvpn_url;
use crate::iclass::IClassApi;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct AssignmentItem {
    pub source:      String,
    pub course_name: String,
    pub title:       String,
    pub start_time:  Option<String>,
    pub due_time:    Option<String>,
    pub score:       Option<String>,
    pub status:      String,
}

impl IClassApi {
    pub async fn get_assignments(&self) -> Result<Vec<AssignmentItem>> {

        let judge = self.get_judge_assignments().await;

        let spoc = self.get_spoc_assignments().await;

        match (judge, spoc) {
            (Ok(mut judge), Ok(mut spoc)) => {

                judge.append(&mut spoc);

                judge.sort_by(|left, right| left.due_time.cmp(&right.due_time));

                Ok(judge)
            }
            (Ok(judge), Err(error)) => {

                eprintln!("SPOC 作业暂时不可用: {error}");

                Ok(judge)
            }
            (Err(error), Ok(spoc)) => {

                eprintln!("希冀作业暂时不可用: {error}");

                Ok(spoc)
            }
            (Err(judge), Err(spoc)) => bail!("希冀和 SPOC 作业都加载失败: {judge}; {spoc}"),
        }
    }

    pub async fn get_judge_assignments(&self) -> Result<Vec<AssignmentItem>> {

        let service_url = academic_judge_url(
            self.use_vpn,
            "https://sso.buaa.edu.cn/login?service=http%3A%2F%2Fjudge.buaa.edu.cn%2F",
        );

        let login = self
            .client
            .get(service_url)
            .send()
            .await
            .context("希冀登录跳转失败")?;

        let login_body = login.text().await.context("读取希冀登录结果失败")?;

        if login_body.contains("name=\"execution\"") || login_body.contains("统一身份认证") {

            bail!("希冀登录状态已失效，请重新登录");
        }

        let courses_url = academic_judge_url(
            self.use_vpn,
            "https://judge.buaa.edu.cn/courselist.jsp?courseID=0",
        );

        let courses_response = self
            .client
            .get(courses_url)
            .header("User-Agent", judge_user_agent())
            .send()
            .await
            .context("获取希冀课程列表失败")?;

        let courses_body = courses_response
            .text()
            .await
            .context("读取希冀课程列表失败")?;

        let courses = parse_course_links(&courses_body);

        let mut assignments = Vec::new();

        for (course_id, course_name) in courses {

            let course_url = academic_judge_url(
                self.use_vpn,
                &format!("https://judge.buaa.edu.cn/courselist.jsp?courseID={course_id}"),
            );

            let _ = self
                .client
                .get(course_url)
                .header("User-Agent", judge_user_agent())
                .send()
                .await
                .context("选择希冀课程失败")?;

            let assignment_url = academic_judge_url(
                self.use_vpn,
                "https://judge.buaa.edu.cn/assignment/index.jsp",
            );

            let response = self
                .client
                .get(assignment_url)
                .header("User-Agent", judge_user_agent())
                .send()
                .await
                .context("获取希冀作业列表失败")?;

            let body = response.text().await.context("读取希冀作业列表失败")?;

            for assignment_id in parse_assignment_ids(&body) {

                let detail_url = academic_judge_url(
                    self.use_vpn,
                    &format!(
                        "https://judge.buaa.edu.cn/assignment/index.jsp?assignID={assignment_id}"
                    ),
                );

                let detail = self
                    .client
                    .get(detail_url)
                    .header("User-Agent", judge_user_agent())
                    .send()
                    .await
                    .context("获取希冀作业详情失败")?
                    .text()
                    .await
                    .context("读取希冀作业详情失败")?;

                assignments.push(parse_assignment_detail(&detail, &course_name));
            }
        }

        assignments.sort_by(|left, right| {

            left.due_time
                .as_deref()
                .unwrap_or("9999")
                .cmp(right.due_time.as_deref().unwrap_or("9999"))
                .then_with(|| left.course_name.cmp(&right.course_name))
                .then_with(|| left.title.cmp(&right.title))
        });

        Ok(assignments)
    }

    pub async fn get_spoc_assignments(&self) -> Result<Vec<AssignmentItem>> {

        let mut current_url =
            academic_judge_url(self.use_vpn, "https://spoc.buaa.edu.cn/spocnewht/cas");

        let token = loop {

            let response = self
                .no_redirect_client
                .get(&current_url)
                .send()
                .await
                .context("SPOC 登录跳转失败")?;

            if let Some(token) = response
                .url()
                .query_pairs()
                .find(|(key, _)| key == "token")
                .map(|(_, value)| value.to_string())
                .filter(|value| !value.is_empty())
            {

                break token;
            }

            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| anyhow::anyhow!("SPOC 登录跳转未返回 token"))?;

            current_url = reqwest::Url::parse(&current_url)
                .and_then(|base| base.join(location))
                .map(|url| url.to_string())
                .context("解析 SPOC 登录跳转失败")?;
        };

        let login_url = academic_judge_url(
            self.use_vpn,
            "https://spoc.buaa.edu.cn/spocnewht/sys/casLogin",
        );

        let login = self
            .client
            .post(login_url)
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Token", format!("Inco-{token}"))
            .json(&serde_json::json!({"token": token}))
            .send()
            .await
            .context("SPOC 登录失败")?;

        let login_body: Value = login.json().await.context("SPOC 登录响应格式异常")?;

        let role = login_body
            .pointer("/content/jsdm")
            .and_then(Value::as_str)
            .or_else(|| {

                login_body
                    .pointer("/content/rolecode/0")
                    .and_then(Value::as_str)
            })
            .ok_or_else(|| anyhow::anyhow!("SPOC 登录响应缺少角色信息"))?
            .to_string();

        let current_term = self
            .client
            .post(academic_judge_url(
                self.use_vpn,
                "https://spoc.buaa.edu.cn/spocnewht/inco/ht/queryOne",
            ))
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Token", format!("Inco-{token}"))
            .header("RoleCode", &role)
            .json(&serde_json::json!({"param": spoc_encrypt_param(SPOC_CURRENT_TERM_PARAM)}))
            .send()
            .await
            .context("获取 SPOC 当前学期失败")?;

        let current_body: Value = current_term.json().await.context("SPOC 学期响应格式异常")?;

        let term_code = current_body
            .pointer("/content/mrxq")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("SPOC 未返回当前学期"))?;

        let mut page = 1;

        let mut assignments = Vec::new();

        loop {

            let plain = serde_json::json!({
                "pageSize": 15,
                "pageNum": page,
                "sqlid": SPOC_ASSIGNMENTS_SQL_ID,
                "xnxq": term_code,
                "kcid": "",
                "yzwz": ""
            });

            let response = self
                .client
                .post(academic_judge_url(
                    self.use_vpn,
                    "https://spoc.buaa.edu.cn/spocnewht/inco/ht/queryListByPage",
                ))
                .header("X-Requested-With", "XMLHttpRequest")
                .header("Token", format!("Inco-{token}"))
                .header("RoleCode", &role)
                .json(&serde_json::json!({"param": spoc_encrypt_param(&plain.to_string())}))
                .send()
                .await
                .context("获取 SPOC 作业失败")?;

            let body: Value = response.json().await.context("SPOC 作业响应格式异常")?;

            let content = body.get("content").cloned().unwrap_or_default();

            let list = content
                .get("list")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();

            let list_empty = list.is_empty();

            for row in &list {

                assignments.push(AssignmentItem {
                    source:      "spoc".to_string(),
                    course_name: text_at(&row, &["kcmc", "courseName"]).unwrap_or_default(),
                    title:       text_at(&row, &["zymc", "title"])
                        .unwrap_or_else(|| "SPOC 作业".to_string()),
                    start_time:  text_at(&row, &["zykssj", "startTime"]),
                    due_time:    text_at(&row, &["zyjzsj", "dueTime"]),
                    score:       text_at(&row, &["mf", "score"]),
                    status:      text_at(&row, &["tjzt", "submissionStatus"])
                        .unwrap_or_else(|| "未知".to_string()),
                });
            }

            let has_next = content
                .get("hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            let pages = content
                .get("pages")
                .and_then(Value::as_u64)
                .unwrap_or(page as u64);

            if !has_next || page as u64 >= pages || list_empty {

                break;
            }

            page += 1;
        }

        assignments.sort_by(|left, right| left.due_time.cmp(&right.due_time));

        Ok(assignments)
    }
}

const SPOC_CURRENT_TERM_PARAM: &str =
    "YHrxtTavu6raCwC0/qdgYffB9evWHBkTng/XS4W6j3f/TPo02iEPSoegscDTRNzIPRG49o3RHl4JiFCXAiBkkA==";

const SPOC_ASSIGNMENTS_SQL_ID: &str = "1713252980496efac7d5d9985e81693116d3e8a52ebf2b";

fn spoc_encrypt_param(value: &str) -> String {

    const KEY: &[u8; 16] = b"inco12345678ocni";

    const IV: &[u8; 16] = b"ocni12345678inco";

    let bytes = value.as_bytes();

    let padded_len = bytes.len().next_multiple_of(16);

    let mut padded = vec![0_u8; padded_len];

    padded[..bytes.len()].copy_from_slice(bytes);

    let cipher = Aes128::new(&Array::from(*KEY));

    let mut previous = *IV;

    let mut encrypted = Vec::with_capacity(padded_len);

    for chunk in padded.chunks_exact(16) {

        let mut block = Array::from(array_xor(chunk, &previous));

        cipher.encrypt_block(&mut block);

        previous = block.into();

        encrypted.extend_from_slice(&block);
    }

    BASE64.encode(encrypted)
}

fn array_xor(left: &[u8], right: &[u8; 16]) -> [u8; 16] {

    let mut output = [0_u8; 16];

    for index in 0..16 {

        output[index] = left[index] ^ right[index];
    }

    output
}

fn text_at(row: &Value, keys: &[&str]) -> Option<String> {

    keys.iter().find_map(|key| {

        match row.get(*key)? {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        }
    })
}

fn academic_judge_url(use_vpn: bool, raw: &str) -> String {

    if use_vpn {

        to_webvpn_url(raw)
    } else {

        raw.to_string()
    }
}

fn parse_course_links(body: &str) -> Vec<(String, String)> {

    let document = Html::parse_document(body);

    let selector =
        Selector::parse(r#"a[href*="courselist.jsp?courseID="]"#).expect("valid course selector");

    document
        .select(&selector)
        .filter_map(|node| {

            let href = node.value().attr("href")?;

            let id = href
                .split("courseID=")
                .nth(1)?
                .split('&')
                .next()?
                .to_string();

            if id == "0" || id.is_empty() {

                return None;
            }

            let name = node.text().collect::<String>().trim().to_string();

            (!name.is_empty()).then_some((id, name))
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn parse_assignment_ids(body: &str) -> Vec<String> {

    let document = Html::parse_document(body);

    let selector = Selector::parse(r#"a[href*="assignID="]"#).expect("valid assignment selector");

    document
        .select(&selector)
        .filter_map(|node| {

            let href = node.value().attr("href")?;

            if href.contains("problemContent") || href.contains("judgeDetails") {

                return None;
            }

            Some(
                href.split("assignID=")
                    .nth(1)?
                    .split('&')
                    .next()?
                    .to_string(),
            )
        })
        .filter(|id| !id.is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn parse_assignment_detail(body: &str, course_name: &str) -> AssignmentItem {

    let document = Html::parse_document(body);

    let text = document.root_element().text().collect::<Vec<_>>().join(" ");

    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");

    let title = text
        .split("作业时间")
        .next()
        .unwrap_or_default()
        .trim()
        .rsplit_once(' ')
        .map(|(_, value)| value.to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "希冀作业".to_string());

    let (start_time, due_time) = text
        .split_once("作业时间")
        .and_then(|(_, rest)| rest.split_once('至'))
        .map(|(start, end)| (extract_datetime(start), extract_datetime(end)))
        .unwrap_or((None, None));

    let score = text
        .split("总分")
        .nth(1)
        .and_then(|value| value.split_whitespace().next())
        .map(|value| value.trim_matches([':', '：']).to_string());

    let status = if text.contains("未提交") || text.contains("未作答") {

        "未提交"
    } else if text.contains("已提交") || text.contains("Accepted") || text.contains("得分") {

        "已提交"
    } else {

        "未知"
    };

    AssignmentItem {
        source: "judge".to_string(),
        course_name: course_name.to_string(),
        title,
        start_time,
        due_time,
        score,
        status: status.to_string(),
    }
}

fn extract_datetime(value: &str) -> Option<String> {

    let parts = value.split_whitespace().collect::<Vec<_>>();

    parts.windows(2).find_map(|window| {

        let date = window[0].trim_matches([':', '：']);

        let time = window[1].trim_matches([':', '：']);

        (date.len() == 10
            && date.as_bytes().get(4) == Some(&b'-')
            && time.len() >= 5
            && time.as_bytes().get(2) == Some(&b':'))
        .then(|| format!("{date} {time}"))
    })
}

fn judge_user_agent() -> &'static str {

    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/138 Safari/537.36"
}

#[cfg(test)]

mod tests {

    use super::{parse_assignment_detail, parse_assignment_ids, parse_course_links};

    #[test]

    fn parses_judge_course_and_assignment_links() {

        let courses = parse_course_links(r#"<a href="courselist.jsp?courseID=12">数据结构</a>"#);

        assert_eq!(courses, vec![("12".to_string(), "数据结构".to_string())]);

        assert_eq!(
            parse_assignment_ids(r#"<a href="assignment/index.jsp?assignID=9">作业一</a>"#),
            vec!["9"]
        );
    }

    #[test]

    fn parses_judge_assignment_summary_text() {

        let item = parse_assignment_detail(
            "<html>作业一 作业时间：2026-09-01 08:00 至 2026-09-10 23:59 总分：100 未提交</html>",
            "数据结构",
        );

        assert_eq!(item.course_name, "数据结构");

        assert_eq!(item.status, "未提交");

        assert_eq!(item.due_time.as_deref(), Some("2026-09-10 23:59"));
    }
}
