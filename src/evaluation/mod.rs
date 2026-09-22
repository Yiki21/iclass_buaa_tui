//! Course evaluation (评教) for `spoc.buaa.edu.cn/pjxt`.
//!
//! Why:
//! End-of-term evaluation is mandatory before grades are released, and doing it
//! through the web UI for every course is tedious. This module reads the
//! outstanding tasks and can submit them.
//!
//! How:
//! The session is activated by visiting the evaluation CAS entry with the shared
//! login. Listing tasks is read-only. Submitting builds one result object per
//! evaluated party, answering each question with an option chosen from the
//! questionnaire.
//!
//! # What submitting does
//!
//! Submission answers the questionnaire **on the user's behalf, without the
//! user reading the questions**, and the result is attributed to them
//! permanently. It is therefore never automatic: the CLI prints exactly what
//! will be sent and requires an explicit confirmation flag.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::constants::to_webvpn_url;
use crate::iclass::IClassApi;

const PJXT_BASE: &str = "https://spoc.buaa.edu.cn/pjxt";

/// CAS entry that activates the evaluation session.

const CAS_URL: &str = "https://spoc.buaa.edu.cn/pjxt/cas";

/// One course awaiting evaluation.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct EvaluationTask {
    /// Task id, required by every later call.
    pub rwid:        String,
    /// Questionnaire id.
    pub wjid:        String,
    pub course:      String,
    pub course_code: String,
    pub teacher:     String,
    /// Whether this course has already been evaluated.
    pub evaluated:   bool,
}

/// One question inside a questionnaire.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct Question {
    pub id:      String,
    pub text:    String,
    /// Option ids, in the order the questionnaire offers them.
    pub options: Vec<String>,
    /// Whether the question is multiple choice.
    pub choice:  bool,
}

/// A questionnaire, flattened to its questions.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]

pub struct Questionnaire {
    pub questions: Vec<Question>,
}

impl Questionnaire {
    /// Answers every question with its first option.
    ///
    /// Why:
    /// The first option is conventionally the most favourable, matching what
    /// the reference implementation sends.
    ///
    /// How:
    /// Returns question-id to option-id pairs. Non-choice questions and those
    /// without options are omitted.

    pub fn default_answers(&self) -> Vec<(String, String)> {

        self.questions
            .iter()
            .filter(|question| question.choice)
            .filter_map(|question| {

                question
                    .options
                    .first()
                    .map(|option| (question.id.clone(), option.clone()))
            })
            .collect()
    }
}

/// Outcome for one course.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct EvaluationOutcome {
    pub course:  String,
    pub success: bool,
    pub message: String,
}

impl IClassApi {
    /// Activates the evaluation session with the shared login.

    async fn evaluation_activate(&self) -> Result<()> {

        let response = self
            .client
            .get(pjxt_url(self.use_vpn, CAS_URL))
            .send()
            .await
            .context("评教 CAS 激活失败")?;

        let status = response.status().as_u16();

        let final_url = response.url().to_string();

        let body = response.text().await.context("读取评教 CAS 响应失败")?;

        if status == 401 || final_url.contains("sso.buaa.edu.cn") || body.contains("统一身份认证")
        {

            bail!("评教登录状态已失效，请先完成统一认证登录");
        }

        if !(200..300).contains(&status) {

            bail!("评教会话激活失败: HTTP {status}");
        }

        Ok(())
    }

    /// Lists the courses awaiting evaluation.
    ///
    /// How:
    /// The endpoint needs the student id, which the session carries.

    pub async fn evaluation_tasks(&self, user_id: &str) -> Result<Vec<EvaluationTask>> {

        self.evaluation_activate().await?;

        let response = self
            .client
            .get(pjxt_url(
                self.use_vpn,
                &format!("{PJXT_BASE}/personnelEvaluation/listObtainPersonnelEvaluationTasks"),
            ))
            .query(&[("yhdm", user_id), ("pageNum", "1"), ("pageSize", "100")])
            .header("X-Requested-With", "XMLHttpRequest")
            .send()
            .await
            .context("获取待评教列表失败")?;

        let body = response.text().await.context("读取待评教列表失败")?;

        let value = unwrap_envelope(&body)?;

        let rows = value
            .get("list")
            .or_else(|| value.get("records"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        Ok(rows.iter().filter_map(parse_task).collect())
    }

    /// Loads a course's questionnaire.

    pub async fn evaluation_questionnaire(&self, task: &EvaluationTask) -> Result<Questionnaire> {

        let response = self
            .client
            .get(pjxt_url(
                self.use_vpn,
                &format!("{PJXT_BASE}/evaluationMethodSix/getQuestionnaireTopic"),
            ))
            .query(&[
                ("id", ""),
                ("rwid", task.rwid.as_str()),
                ("wjid", task.wjid.as_str()),
            ])
            .header("X-Requested-With", "XMLHttpRequest")
            .send()
            .await
            .context("获取评教问卷失败")?;

        let body = response.text().await.context("读取评教问卷失败")?;

        let value = unwrap_envelope(&body)?;

        Ok(parse_questionnaire(&value))
    }

    /// Submits the evaluation for one course.
    ///
    /// Why:
    /// This permanently records the user's answers. The caller must have
    /// confirmed explicitly; see the CLI, which refuses without `--yes`.
    ///
    /// How:
    /// Every party listed for the course gets one result object, each answering
    /// the questionnaire as prepared.

    pub async fn evaluation_submit(
        &self,
        task: &EvaluationTask,
        answers: &[(String, String)],
    ) -> Result<EvaluationOutcome> {

        self.evaluation_activate().await?;

        if answers.is_empty() {

            bail!("问卷没有可作答的题目，无法提交");
        }

        // Switch the questionnaire into its reviewable pattern first; without
        // it the submission is rejected.
        let _ = self
            .client
            .post(pjxt_url(
                self.use_vpn,
                &format!("{PJXT_BASE}/evaluationMethodSix/reviseQuestionnairePattern"),
            ))
            .header("Content-Type", "application/json")
            .body(
                json!({
                    "rwid": task.rwid,
                    "wjid": task.wjid,
                    "msid": "1",
                })
                .to_string(),
            )
            .send()
            .await;

        let results = build_payload(task, answers);

        let response = self
            .client
            .post(pjxt_url(
                self.use_vpn,
                &format!("{PJXT_BASE}/evaluationMethodSix/submitSaveEvaluation"),
            ))
            .header("Content-Type", "application/json")
            .body(
                json!({
                    "pjidlist": [],
                    "pjjglist": results,
                    "pjzt": "1",
                })
                .to_string(),
            )
            .send()
            .await
            .context("提交评教失败")?;

        let body = response.text().await.context("读取评教提交响应失败")?;

        match unwrap_envelope(&body) {
            Ok(_) => {
                Ok(EvaluationOutcome {
                    course:  task.course.clone(),
                    success: true,
                    message: "评教成功".to_string(),
                })
            }

            Err(error) => {
                Ok(EvaluationOutcome {
                    course:  task.course.clone(),
                    success: false,
                    message: format!("提交失败: {error}"),
                })
            }
        }
    }
}

/// Builds the result objects sent to the submission endpoint.
///
/// Why:
/// The service expects one object per evaluated party, each carrying the full
/// questionnaire answer set. The shape here mirrors what the web client sends,
/// including the fixed score and flags, because the service validates them.

fn build_payload(task: &EvaluationTask, answers: &[(String, String)]) -> Vec<Value> {

    let answers: Vec<Value> = answers
        .iter()
        .map(|(question_id, option_id)| {

            json!({
                "sjly": "1",
                "stlx": "1",
                "wjid": task.wjid,
                "wjstctid": "",
                "wjstid": question_id,
                "xxdalist": [option_id],
            })
        })
        .collect();

    vec![json!({
        "kcdm": task.course_code,
        "kcmc": task.course,
        // The score the reference client sends for a favourable evaluation.
        "pjdf": 93,
        "pjfs": "1",
        "pjlx": null,
        "pjrjsdm": "",
        "pjsx": 1,
        "pjxxlist": answers,
        "stzjid": "xx",
        "wjid": task.wjid,
        "wtjjy": "",
        "xnxq": null,
        "sfxxpj": "1",
        "sfnm": "1",
    })]
}

fn pjxt_url(use_vpn: bool, raw: &str) -> String {

    if use_vpn {

        to_webvpn_url(raw)
    } else {

        raw.to_string()
    }
}

/// Unwraps the service's envelope, which signals failure inside a 200 response.

fn unwrap_envelope(body: &str) -> Result<Value> {

    let value: Value = serde_json::from_str(body).with_context(|| {

        let preview: String = body.chars().take(120).collect();

        format!("评教服务返回了非 JSON 响应: {preview}")
    })?;

    if let Some(code) = value.get("code") {

        let succeeded = code
            .as_i64()
            .map(|number| matches!(number, 0 | 1 | 200))
            .or_else(|| {

                code.as_str()
                    .map(|text| matches!(text, "0" | "1" | "200" | "success"))
            })
            .unwrap_or(true);

        if !succeeded {

            let message = value
                .get("message")
                .or_else(|| value.get("msg"))
                .and_then(Value::as_str)
                .unwrap_or("评教服务返回错误");

            bail!("{message}");
        }
    }

    Ok(value.get("result").cloned().unwrap_or(value))
}

fn parse_task(row: &Value) -> Option<EvaluationTask> {

    let rwid = row
        .get("rwid")
        .or_else(|| row.get("wjssrwid"))
        .and_then(flexible_string)
        .filter(|value| !value.is_empty())?;

    let wjid = row
        .get("wjid")
        .and_then(flexible_string)
        .unwrap_or_default();

    Some(EvaluationTask {
        rwid,
        wjid,
        course: row
            .get("kcmc")
            .or_else(|| row.get("courseName"))
            .and_then(flexible_string)
            .unwrap_or_else(|| "未知课程".to_string()),
        course_code: row
            .get("kcdm")
            .and_then(flexible_string)
            .unwrap_or_default(),
        teacher: row
            .get("jsmc")
            .or_else(|| row.get("pjrmc"))
            .and_then(flexible_string)
            .unwrap_or_default(),
        evaluated: row
            .get("sfypj")
            .or_else(|| row.get("isEvaluated"))
            .and_then(|value| {

                value
                    .as_bool()
                    .or_else(|| value.as_i64().map(|number| number == 1))
                    .or_else(|| value.as_str().map(|text| text == "1" || text == "true"))
            })
            .unwrap_or(false),
    })
}

/// Flattens a questionnaire response into questions.
///
/// How:
/// The service nests questions three deep: the entity holds sections, each
/// section holds a question list. Both the nested and the flat shapes are
/// accepted, since the response varies by questionnaire type.

fn parse_questionnaire(value: &Value) -> Questionnaire {

    let entity = value
        .get("pjxtWjWjbReturnEntity")
        .or_else(|| value.get("wjxx"))
        .unwrap_or(value);

    let mut questions = Vec::new();

    if let Some(sections) = entity.get("wjzblist").and_then(Value::as_array) {

        for section in sections {

            let Some(rows) = section.get("tklist").and_then(Value::as_array) else {

                continue;
            };

            for row in rows {

                if let Some(question) = parse_question(row) {

                    questions.push(question);
                }
            }
        }
    } else if let Some(rows) = entity.get("tklist").and_then(Value::as_array) {

        for row in rows {

            if let Some(question) = parse_question(row) {

                questions.push(question);
            }
        }
    }

    Questionnaire { questions }
}

fn parse_question(row: &Value) -> Option<Question> {

    let id = row
        .get("tmid")
        .and_then(flexible_string)
        .filter(|value| !value.is_empty())?;

    // `tmlx` is "1" for multiple choice; anything else is free text, which has
    // no options to select.
    let kind = row
        .get("tmlx")
        .and_then(flexible_string)
        .unwrap_or_default();

    let options = row
        .get("tmxxlist")
        .and_then(Value::as_array)
        .map(|rows| {

            rows.iter()
                .filter_map(|option| option.get("tmxxid").and_then(flexible_string))
                .collect()
        })
        .unwrap_or_default();

    Some(Question {
        id,
        text: row
            .get("tmmc")
            .or_else(|| row.get("tmnr"))
            .and_then(flexible_string)
            .unwrap_or_default(),
        choice: kind == "1",
        options,
    })
}

fn flexible_string(value: &Value) -> Option<String> {

    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|number| number.to_string()))
}

#[cfg(test)]

mod tests {

    use super::{
        Question, Questionnaire, build_payload, parse_questionnaire, parse_task, unwrap_envelope,
    };
    use serde_json::json;

    #[test]

    fn parses_a_nested_questionnaire() {

        let response = json!({
            "pjxtWjWjbReturnEntity": {
                "wjzblist": [
                    {
                        "tkmc": "第一部分",
                        "tklist": [
                            {
                                "tmid": "q1",
                                "tmlx": "1",
                                "tmmc": "教学态度",
                                "tmxxlist": [
                                    {"tmxxid": "o1", "tmxxmc": "非常满意"},
                                    {"tmxxid": "o2", "tmxxmc": "满意"}
                                ]
                            },
                            {
                                "tmid": "q2",
                                "tmlx": "6",
                                "tmmc": "意见建议"
                            }
                        ]
                    }
                ]
            }
        });

        let questionnaire = parse_questionnaire(&response);

        assert_eq!(questionnaire.questions.len(), 2);

        assert_eq!(questionnaire.questions[0].id, "q1");

        assert!(questionnaire.questions[0].choice);

        assert_eq!(questionnaire.questions[0].options, vec!["o1", "o2"]);

        assert!(!questionnaire.questions[1].choice, "非选择题不应作答");

        assert!(questionnaire.questions[1].options.is_empty());
    }

    #[test]

    fn default_answers_pick_the_first_option_of_choices_only() {

        let questionnaire = Questionnaire {
            questions: vec![
                Question {
                    id:      "q1".to_string(),
                    text:    "教学".to_string(),
                    options: vec!["o1".to_string(), "o2".to_string()],
                    choice:  true,
                },
                Question {
                    id:      "q2".to_string(),
                    text:    "建议".to_string(),
                    options: Vec::new(),
                    choice:  false,
                },
                // A choice question with no options cannot be answered.
                Question {
                    id:      "q3".to_string(),
                    text:    "空题".to_string(),
                    options: Vec::new(),
                    choice:  true,
                },
            ],
        };

        let answers = questionnaire.default_answers();

        assert_eq!(answers, vec![("q1".to_string(), "o1".to_string())]);
    }

    #[test]

    fn payload_carries_one_result_per_question() {

        let task = super::EvaluationTask {
            rwid:        "r1".to_string(),
            wjid:        "w1".to_string(),
            course:      "高等数学".to_string(),
            course_code: "MATH101".to_string(),
            teacher:     "张老师".to_string(),
            evaluated:   false,
        };

        let answers = vec![
            ("q1".to_string(), "o1".to_string()),
            ("q2".to_string(), "o2".to_string()),
        ];

        let payload = build_payload(&task, &answers);

        assert_eq!(payload.len(), 1, "每门课提交一条结果");

        let entry = &payload[0];

        assert_eq!(entry["kcmc"], json!("高等数学"));

        assert_eq!(entry["wjid"], json!("w1"));

        assert_eq!(entry["pjxxlist"].as_array().map(Vec::len), Some(2));

        assert_eq!(entry["pjxxlist"][0]["wjstid"], json!("q1"));

        assert_eq!(entry["pjxxlist"][0]["xxdalist"], json!(["o1"]));
    }

    #[test]

    fn parses_a_task_row() {

        let task = parse_task(&json!({
            "rwid": "r1",
            "wjid": "w1",
            "kcmc": "线性代数",
            "kcdm": "MATH102",
            "pjrmc": "李老师",
            "sfypj": "1"
        }))
        .expect("应能解析评教任务");

        assert_eq!(task.course, "线性代数");

        assert!(task.evaluated, "sfypj=1 表示已评");

        // A row without a task id cannot be acted on.
        assert!(parse_task(&json!({"kcmc": "无 id"})).is_none());
    }

    #[test]

    fn envelope_failures_are_surfaced() {

        assert!(unwrap_envelope(r#"{"code":0,"result":{"a":1}}"#).is_ok());

        let error = unwrap_envelope(r#"{"code":500,"message":"服务器错误"}"#)
            .unwrap_err()
            .to_string();

        assert!(error.contains("服务器错误"), "应保留服务端消息: {error}");
    }

    #[test]

    fn envelope_result_is_unwrapped() {

        let value = unwrap_envelope(r#"{"code":0,"result":{"list":[1,2]}}"#).expect("应能解析");

        assert_eq!(value["list"].as_array().map(Vec::len), Some(2));
    }
}
