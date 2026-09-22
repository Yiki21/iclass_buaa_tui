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
    /// Upstream identifier, needed to fetch the detail page on demand.
    ///
    /// Why:
    /// SPOC's list endpoint carries no problem breakdown; only its detail
    /// endpoint does. Keeping the id lets the UI fetch that lazily for the one
    /// assignment the user opened instead of for every row.
    pub id:          String,
    pub course_name: String,
    pub title:       String,
    pub start_time:  Option<String>,
    pub due_time:    Option<String>,
    pub score:       Option<String>,
    pub status:      String,
    /// Full mark for the assignment, when the page states one.
    pub max_score:   Option<String>,
    /// How many problems have been submitted, and how many exist.
    ///
    /// Why:
    /// "Submitted" hides whether one problem out of six is still open, which
    /// is exactly the thing a student needs to see.
    pub submitted:   usize,
    pub total:       usize,
    /// Per-problem breakdown, empty when the page has no problem table.
    pub problems:    Vec<AssignmentProblem>,
}

/// One row of a Judge assignment's problem table.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct AssignmentProblem {
    pub name:      String,
    pub score:     Option<String>,
    pub max_score: Option<String>,
    pub status:    String,
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

                let mut item = parse_assignment_detail(&detail, &course_name);

                item.id = assignment_id.clone();

                assignments.push(item);
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

    /// Performs the SPOC CAS handshake and returns `(token, role)`.
    ///
    /// Why:
    /// Every SPOC request needs both headers, and the list and detail fetches
    /// used to inline the same handshake. One place means one bug fix.

    async fn spoc_login(&self) -> Result<(String, String)> {

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

        Ok((token, role))
    }

    pub async fn get_spoc_assignments(&self) -> Result<Vec<AssignmentItem>> {

        let (token, role) = self.spoc_login().await?;

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
                    source: "spoc".to_string(),
                    course_name: text_at(&row, &["kcmc", "courseName"]).unwrap_or_default(),
                    title: text_at(&row, &["zymc", "title"])
                        .unwrap_or_else(|| "SPOC 作业".to_string()),
                    start_time: text_at(&row, &["zykssj", "startTime"]),
                    due_time: text_at(&row, &["zyjzsj", "dueTime"]),
                    score: text_at(&row, &["df", "score", "mf"]),
                    status: text_at(&row, &["tjzt", "submissionStatus"])
                        .unwrap_or_else(|| "未知".to_string()),
                    ..AssignmentItem::default()
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

    /// Fetches one SPOC assignment's detail, including its description.
    ///
    /// Why:
    /// The list endpoint returns no body text; only the detail endpoint does.
    /// Calling it lazily for the assignment the user opened keeps the list load
    /// to one page request instead of one per row.

    pub async fn get_spoc_assignment_detail(
        &self,
        assignment_id: &str,
    ) -> Result<SpocAssignmentDetail> {

        if assignment_id.trim().is_empty() {

            bail!("缺少 SPOC 作业 id，无法获取详情");
        }

        let (token, role) = self.spoc_login().await?;

        let response = self
            .client
            .get(academic_judge_url(
                self.use_vpn,
                "https://spoc.buaa.edu.cn/spocnewht/kczy/queryKczyInfoByid",
            ))
            .query(&[("id", assignment_id)])
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Token", format!("Inco-{token}"))
            .header("RoleCode", &role)
            .send()
            .await
            .context("获取 SPOC 作业详情失败")?;

        let body: Value = response.json().await.context("SPOC 作业详情响应格式异常")?;

        let content = body.get("content").cloned().unwrap_or_default();

        let html = text_at(&content, &["zynr"]).unwrap_or_default();

        Ok(SpocAssignmentDetail {
            title:       text_at(&content, &["zymc"]).unwrap_or_default(),
            start_time:  text_at(&content, &["zykssj"]),
            due_time:    text_at(&content, &["zyjzsj"]),
            score:       text_at(&content, &["zyfs"]),
            description: html_to_text(&html),
        })
    }
}

const SPOC_CURRENT_TERM_PARAM: &str =
    "YHrxtTavu6raCwC0/qdgYffB9evWHBkTng/XS4W6j3f/TPo02iEPSoegscDTRNzIPRG49o3RHl4JiFCXAiBkkA==";

const SPOC_ASSIGNMENTS_SQL_ID: &str = "1713252980496efac7d5d9985e81693116d3e8a52ebf2b";

/// One SPOC assignment's detail, with the description reduced to plain text.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct SpocAssignmentDetail {
    pub title:       String,
    pub start_time:  Option<String>,
    pub due_time:    Option<String>,
    pub score:       Option<String>,
    /// Assignment body, HTML stripped. Empty when the assignment has none.
    pub description: String,
}

/// Parses a deadline string into a local timestamp.
///
/// Why:
/// The UIs color a due time by whether it has passed, which needs a real value
/// rather than a string comparison.

pub fn parse_deadline(value: &str) -> Option<chrono::NaiveDateTime> {

    let value = value.trim();

    for format in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y/%m/%d %H:%M:%S",
        "%Y/%m/%d %H:%M",
    ] {

        if let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(value, format) {

            return Some(parsed);
        }
    }

    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(23, 59, 59))
}

/// Strips tags from an HTML fragment and collapses its whitespace.
///
/// Why:
/// The description arrives as HTML markup. Rendering it raw would pour tag
/// soup into the terminal, so it is reduced to readable text.

fn html_to_text(html: &str) -> String {

    if html.trim().is_empty() {

        return String::new();
    }

    // Block-level tags become line breaks so paragraphs do not run together.
    let spaced = html
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("</p>", "\n")
        .replace("</div>", "\n")
        .replace("</li>", "\n");

    let document = Html::parse_fragment(&spaced);

    let text = document.root_element().text().collect::<Vec<_>>().join("");

    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

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

    // Join the page's text with single spaces everywhere, not just at ASCII
    // whitespace. Judge writes "作业满分：100" with no space, so splitting on
    // whitespace alone leaves the label glued to its value and every
    // `split("作业满分")` lookup fails.
    let text = normalize_page_text(&document.root_element().text().collect::<Vec<_>>().join(" "));

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

    // The value follows the label as ": 58", so the colon must go before the
    // first whitespace-delimited token is taken.
    let score = labeled_value(&text, "总分");

    // "作业满分: 100" is stated on the page; keeping it lets the UI show
    // 85 / 100 instead of a bare 85.
    let max_score = labeled_value(&text, "作业满分");

    let problems = parse_problem_rows(&document);

    let submitted = problems
        .iter()
        .filter(|problem| problem.status != "未提交")
        .count();

    // The page states the problem count; fall back to the parsed rows when it
    // does not, so the ratio is still meaningful.
    let total = text
        .split('共')
        .nth(1)
        .and_then(|value| value.split('道').next())
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(problems.len());

    let status = if total > 0 && submitted < total {

        "未提交"
    } else if text.contains("未提交") || text.contains("未作答") {

        "未提交"
    } else if text.contains("已提交") || text.contains("Accepted") || text.contains("得分") {

        "已提交"
    } else {

        "未知"
    };

    AssignmentItem {
        source: "judge".to_string(),
        // Filled by the caller, which is where the id is known.
        id: String::new(),
        course_name: course_name.to_string(),
        title,
        start_time,
        due_time,
        score,
        status: status.to_string(),
        max_score,
        submitted,
        total,
        problems,
    }
}

/// Reads the first token following a Chinese label.
///
/// Why:
/// The pages write values as "总分: 58" or "作业满分：100". The separator, its
/// spacing, and the indent all vary, so the colon is stripped and the remainder
/// trimmed before the value is taken.

fn labeled_value(text: &str, label: &str) -> Option<String> {

    let rest = text.split(label).nth(1)?;

    let rest = rest.trim_start_matches([':', '：']);

    let value = rest.split_whitespace().next()?;

    let value = value.trim_end_matches(['分', '.']);

    if value.is_empty() {

        return None;
    }

    Some(value.to_string())
}

/// Collapses a page's text into space-separated words.
///
/// Why:
/// The parsers locate values by splitting on Chinese labels ("作业满分", "总分").
/// Those labels are not ASCII whitespace, so the raw text keeps them attached
/// to the following number and the split never lands where it should. Spacing
/// every run of whitespace, including full-width spaces, makes the labels
/// standalone tokens.

fn normalize_page_text(text: &str) -> String {

    text.split(|character: char| character.is_whitespace() || character == '\u{3000}')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extracts the per-problem table from a Judge assignment page.
///
/// Why:
/// The page lists each problem with its own score and status, which is the only
/// way to tell "one of six still open" from "all done". This is parsed from the
/// page we already download for the list, so it costs no extra request.
///
/// How:
/// Judge nests tables inside tables. Rows are read from the top-level tables
/// only, after nested tables are stripped, so inner layout tables do not
/// contribute spurious rows.

fn parse_problem_rows(document: &Html) -> Vec<AssignmentProblem> {

    let row_selector = Selector::parse("tr").expect("valid row selector");

    let cell_selector = Selector::parse("th, td").expect("valid cell selector");

    // Only the outermost table(s) describe problems.
    let table_selector = Selector::parse("table").expect("valid table selector");

    let mut problems = Vec::new();

    for table in document.select(&table_selector) {

        // A table containing another table is a container, not the problem list.
        if table.select(&table_selector).next().is_some() {

            continue;
        }

        for row in table.select(&row_selector) {

            let cells = row
                .select(&cell_selector)
                .map(|cell| {

                    cell.text()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect::<Vec<_>>();

            if cells.len() < 2 {

                continue;
            }

            let joined = cells.join(" ");

            let Some(status) = detect_problem_status(&joined) else {

                continue;
            };

            // The conventional shape is 序号 | 题目 | 满分 | 状态..., but two
            // column layouts also appear, where the cell already reads
            // "第1题 ... 状态".
            let (name, max_score) = if cells.len() >= 3 {

                let name = cells[1].trim().to_string();

                let max = cells[2]
                    .trim()
                    .trim_end_matches('分')
                    .parse::<f64>()
                    .ok()
                    .map(|value| trim_number(value));

                (name, max)
            } else {

                (cells[0].trim().to_string(), None)
            };

            if name.is_empty() {

                continue;
            }

            problems.push(AssignmentProblem {
                name,
                score: parse_earned_score(&joined).map(trim_number),
                max_score,
                status,
            });
        }
    }

    problems
}

/// Classifies a problem row's status text.
///
/// Why:
/// Judge words the same state several ways ("未提交答案", "还未提交代码"); a
/// single canonical value keeps the rest of the code from string-matching.

fn detect_problem_status(text: &str) -> Option<String> {

    const UNSUBMITTED: [&str; 5] = [
        "还未提交代码",
        "未提交文件",
        "未提交答案",
        "未作答",
        "未提交",
    ];

    const SUBMITTED: [&str; 6] = ["已提交", "得分", "Accepted", "解答正确", "通过", "编译错误"];

    if UNSUBMITTED.iter().any(|marker| text.contains(marker)) {

        return Some("未提交".to_string());
    }

    if SUBMITTED.iter().any(|marker| text.contains(marker)) {

        return Some("已提交".to_string());
    }

    if text.contains("初次提交时间")
        || text.contains("首次提交时间")
        || text.contains("最近一次提交时间")
        || text.contains("最后一次提交时间")
        || text.contains("最后一次修改时间")
    {

        return Some("已提交".to_string());
    }

    None
}

/// Reads an earned score out of a problem row.
///
/// Why:
/// The score appears as "得分: 8" or "8.0/10"; requiring the explicit marker
/// first avoids mistaking the maximum for the earned value.

fn parse_earned_score(text: &str) -> Option<f64> {

    for marker in ["得分", "分数", "成绩"] {

        if let Some(rest) = text.split(marker).nth(1) {

            let candidate: String = rest
                .trim_start_matches([':', '：'])
                .trim_start()
                .chars()
                .take_while(|character| character.is_ascii_digit() || *character == '.')
                .collect();

            if let Ok(value) = candidate.parse::<f64>() {

                return Some(value);
            }
        }
    }

    None
}

/// Drops a trailing `.0` so 85.0 prints as 85.

fn trim_number(value: f64) -> String {

    if (value.fract()).abs() < f64::EPSILON {

        format!("{}", value as i64)
    } else {

        format!("{value}")
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

    use super::{html_to_text, parse_assignment_detail, parse_assignment_ids, parse_course_links};

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

    /// A trimmed copy of the real shape: an outer layout table containing the
    /// problem table, plus the summary line Judge prints above it.

    const JUDGE_PAGE: &str = r#"
<html><body>
<table><tr><td>
  <div>第一次上机作业 作业时间: 2026-09-01 08:00 至 2026-09-10 23:59</div>
  <div>作业满分: 100 共 3 道</div>
  <table>
    <tr><th>序号</th><th>题目</th><th>满分</th><th>状态</th></tr>
    <tr><td>1</td><td>A+B Problem</td><td>40</td><td>得分: 40</td></tr>
    <tr><td>2</td><td>链表反转</td><td>30</td><td>得分: 18</td></tr>
    <tr><td>3</td><td>二叉树遍历</td><td>30</td><td>还未提交答案</td></tr>
  </table>
  <div>总分: 58</div>
</td></tr></table>
</body></html>
"#;

    #[test]

    fn extracts_problem_breakdown_from_judge_detail() {

        let item = parse_assignment_detail(JUDGE_PAGE, "数据结构");

        assert_eq!(item.max_score.as_deref(), Some("100"), "应读出作业满分");

        assert_eq!(item.score.as_deref(), Some("58"), "应读出总分");

        assert_eq!(item.total, 3, "应读出题目总数");

        assert_eq!(item.submitted, 2, "两道已提交");

        assert_eq!(item.problems.len(), 3, "三行题目");

        assert_eq!(item.problems[0].name, "A+B Problem");

        assert_eq!(item.problems[0].score.as_deref(), Some("40"));

        assert_eq!(item.problems[0].max_score.as_deref(), Some("40"));

        assert_eq!(item.problems[0].status, "已提交");

        assert_eq!(item.problems[2].status, "未提交");

        assert_eq!(item.problems[2].score, None, "未提交的题不应编造得分");
    }

    #[test]

    fn in_progress_assignment_is_not_reported_as_submitted() {

        // Two of three problems in: the list view must not call this done,
        // which is the whole reason the breakdown is parsed.
        let item = parse_assignment_detail(JUDGE_PAGE, "数据结构");

        assert_eq!(item.status, "未提交");
    }

    #[test]

    fn judge_detail_without_a_problem_table_still_parses() {

        let page = r#"
<html><body>
<div>期中测验 作业时间: 2026-10-01 09:00 至 2026-10-02 18:00</div>
<div>已提交，得分 88</div>
</body></html>
"#;

        let item = parse_assignment_detail(page, "高等数学");

        assert_eq!(item.due_time.as_deref(), Some("2026-10-02 18:00"));

        assert!(item.problems.is_empty(), "没有题目表时不应编造题目");

        assert_eq!(item.total, 0);

        assert_eq!(item.status, "已提交");
    }

    #[test]

    fn problem_status_markers_are_recognised_in_either_wording() {

        let page = r#"
<html><body>
<table>
  <tr><td>1</td><td>题目一</td><td>10</td><td>还未提交代码</td></tr>
  <tr><td>2</td><td>题目二</td><td>10</td><td>最后一次修改时间 2026-09-05</td></tr>
  <tr><td>3</td><td>题目三</td><td>10</td><td>Accepted</td></tr>
</table>
</body></html>
"#;

        let item = parse_assignment_detail(page, "程序设计");

        assert_eq!(item.problems.len(), 3, "三行都应识别为题目");

        assert_eq!(item.problems[0].status, "未提交");

        assert_eq!(item.problems[1].status, "已提交", "按提交时间识别为已提交");

        assert_eq!(item.problems[2].status, "已提交", "Accepted 识别为已提交");

        assert_eq!(item.submitted, 2);

        assert_eq!(item.total, 3, "页面无“共 N 道”时以题目行数为准");
    }

    #[test]

    fn spoc_description_is_reduced_to_readable_text() {

        let html = r#"<p>请完成<b>第三章</b>习题。</p><div>要求：</div><ul><li>独立完成</li><li>周五前提交</li></ul><br/>附件见课程页。"#;

        let text = html_to_text(html);

        assert_eq!(
            text, "请完成第三章习题。\n要求：\n独立完成\n周五前提交\n附件见课程页。",
            "块级标签应变成换行，行内标签直接去掉"
        );

        assert_eq!(html_to_text("  "), "", "空内容应返回空串而非一堆换行");
    }
}
