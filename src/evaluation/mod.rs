//! Course evaluation (评教) for `spoc.buaa.edu.cn/pjxt`.
//!
//! Why:
//! End-of-term evaluation is mandatory before grades are released, and doing it
//! through the web UI for every course is tedious. This module reads the
//! outstanding courses and can submit them.
//!
//! How:
//! The session is activated by visiting the evaluation CAS entry with the shared
//! login. The service is organised in three levels, and each needs the one
//! above it:
//!
//! 1. an evaluation *round* (`listObtainPersonnelEvaluationTasks`, e.g.
//!    "2026夏季学期学生评教"), which carries no course at all;
//! 2. the *questionnaires* of that round (`getQuestionnaireListToTask`);
//! 3. the *courses* answered with each questionnaire
//!    (`getRequiredReviewsData`), one row per course and evaluated teacher.
//!
//! Only a level-3 row identifies something that can be evaluated, and its
//! fields are exactly what the questionnaire endpoint needs, so each
//! [`EvaluationTask`] keeps that row.
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

/// School and college questions have separate result templates, not separate
/// evaluated people. Empty sources must not be submitted.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub enum QuestionSource {
    #[default]
    #[serde(rename = "1")]
    School,
    #[serde(rename = "2")]
    College,
}

impl QuestionSource {
    fn code(self) -> &'static str {

        match self {
            Self::School => "1",
            Self::College => "2",
        }
    }
}

/// One course (and evaluated teacher) awaiting evaluation.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct EvaluationTask {
    /// Stable handle for this course: `rwid:wjid:kcdm:bpdm`.
    ///
    /// Why:
    /// One evaluation round (`rwid`) covers every course of the term, so the
    /// round id alone cannot name a course.
    pub id:          String,
    /// Evaluation round id.
    pub rwid:        String,
    /// Questionnaire id.
    pub wjid:        String,
    /// Current questionnaire pattern; empty means it has not been selected.
    pub msid:        String,
    pub course:      String,
    pub course_code: String,
    pub teacher:     String,
    /// Whether this course has already been evaluated.
    pub evaluated:   bool,
    /// The course row as the service returned it.
    ///
    /// Why:
    /// The questionnaire endpoint answers "操作失败" unless it receives the
    /// course's own fields (`sxz`, `rwh`, `bpdm`, ...), so they are carried
    /// along rather than re-derived.
    #[serde(default, skip_serializing)]
    pub context:     Value,
}

/// One selectable answer.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]

pub struct QuestionOption {
    pub id:    String,
    /// Text shown to the student, e.g. "优秀".
    pub label: String,
    /// Score the option contributes, when the questionnaire publishes one.
    pub score: Option<f64>,
}

/// One question inside a questionnaire.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]

pub struct Question {
    pub id:       String,
    pub text:     String,
    /// Options, in the order the questionnaire offers them.
    pub options:  Vec<QuestionOption>,
    /// Whether the question is single choice. Other questions are free text
    /// and are left blank.
    pub choice:   bool,
    /// Whether the service requires an answer.
    pub required: bool,
    #[serde(default)]
    pub source:   QuestionSource,
}

impl Question {
    /// Label of one of this question's options, or the id if it has none.

    pub fn option_label(&self, option_id: &str) -> String {

        self.options
            .iter()
            .find(|option| option.id == option_id)
            .map(|option| {
                if option.label.is_empty() {

                    option.id.clone()
                } else {

                    option.label.clone()
                }
            })
            .unwrap_or_else(|| option_id.to_string())
    }
}

/// A questionnaire, flattened to its questions.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]

pub struct Questionnaire {
    pub questions: Vec<Question>,
    /// Result templates for school and college questions, keyed by `sfxxpj`.
    #[serde(skip)]
    pub(crate) parties: Vec<Value>,
    /// Storage keys the service expects echoed in every result.
    #[serde(skip)]
    pub(crate) storage: Value,
    /// School sections excluded from scoring, and the optional section average.
    #[serde(skip)]
    pub(crate) unscored_school_questions: Vec<String>,
    #[serde(skip)]
    pub(crate) school_score_divisor: Option<usize>,
    #[serde(skip)]
    pub(crate) full_score_at_95: bool,
    #[serde(skip)]
    pub(crate) unsupported: Option<String>,
}

impl Questionnaire {
    /// Answers every choice question favourably.
    ///
    /// Why:
    /// The first option is the most favourable. The reference client gives
    /// one question its second option instead, so the answers are not
    /// uniform; that is mirrored here, but deterministically, so the preview
    /// shows exactly what will be sent.
    ///
    /// How:
    /// Returns question-id to option-id pairs. Free-text questions and those
    /// without options are omitted.

    pub fn default_answers(&self) -> Vec<(String, String)> {

        let varied = self
            .questions
            .iter()
            .position(|question| question.choice && question.options.len() > 1);

        self.questions
            .iter()
            .enumerate()
            .filter(|(_, question)| question.choice)
            .filter_map(|(index, question)| {

                let option = if Some(index) == varied {

                    question.options.get(1)
                } else {

                    question.options.first()
                }?;

                Some((question.id.clone(), option.id.clone()))
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

    /// GETs one pjxt endpoint and unwraps its envelope.

    async fn pjxt_get(&self, path: &str, query: &[(&str, &str)], what: &str) -> Result<Value> {

        let response = self
            .client
            .get(pjxt_url(self.use_vpn, &format!("{PJXT_BASE}{path}")))
            .query(query)
            .header("X-Requested-With", "XMLHttpRequest")
            .send()
            .await
            .with_context(|| format!("{what}失败"))?;

        let response = response
            .error_for_status()
            .with_context(|| format!("{what}失败"))?;

        let body = response
            .text()
            .await
            .with_context(|| format!("读取{what}响应失败"))?;

        unwrap_envelope(&body).with_context(|| format!("{what}失败"))
    }

    /// Called only after the user confirms a submission. Re-read the pattern:
    /// the cached list can be stale, including during a batch of submissions.

    async fn evaluation_prepare_pattern(&self, task: &EvaluationTask) -> Result<()> {

        let questionnaires = self
            .pjxt_get(
                "/evaluationMethodSix/getQuestionnaireListToTask",
                &[("rwid", &task.rwid)],
                "核对评教模式",
            )
            .await?;

        if !pattern_needs_confirmation(&questionnaires, &task.wjid)? {

            return Ok(());
        }

        let response = self
            .client
            .post(pjxt_url(
                self.use_vpn,
                &format!("{PJXT_BASE}/evaluationMethodSix/confirmQuestionnairePattern"),
            ))
            .header("Content-Type", "application/json")
            .header("X-Requested-With", "XMLHttpRequest")
            .body(json!({ "rwid": task.rwid, "wjid": task.wjid, "msid": "1" }).to_string())
            .send()
            .await
            .context("初始化评教模式失败")?
            .error_for_status()
            .context("初始化评教模式失败")?;

        let body = response.text().await.context("读取评教模式响应失败")?;

        unwrap_envelope(&body).context("初始化评教模式失败")?;

        Ok(())
    }

    /// Lists every course of the open evaluation rounds, pending first.
    ///
    /// How:
    /// Walks round → questionnaire → course; see the module documentation.

    pub async fn evaluation_tasks(&self, user_id: &str) -> Result<Vec<EvaluationTask>> {

        self.evaluation_activate().await?;

        let rounds = self
            .pjxt_get(
                "/personnelEvaluation/listObtainPersonnelEvaluationTasks",
                &[("yhdm", user_id), ("pageNum", "1"), ("pageSize", "100")],
                "获取评教任务",
            )
            .await?;

        let rounds = rounds
            .get("list")
            .or_else(|| rounds.get("records"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut tasks: Vec<EvaluationTask> = Vec::new();

        for round in &rounds {

            let Some(rwid) = text_field(round, &["rwid"]) else {

                continue;
            };

            let questionnaires = self
                .pjxt_get(
                    "/evaluationMethodSix/getQuestionnaireListToTask",
                    &[("rwid", rwid.as_str())],
                    "获取评教问卷列表",
                )
                .await?;

            for questionnaire in questionnaires.as_array().into_iter().flatten() {

                let Some(wjid) = text_field(questionnaire, &["wjid"]) else {

                    continue;
                };

                let msid = text_field(questionnaire, &["msid"]).unwrap_or_default();

                // The course list is empty unless the term is named; the
                // round carries it.
                let term = text_field(round, &["rwxnxq", "xnxq"]).unwrap_or_default();

                let mut query = vec![("wjid", wjid.as_str())];

                if !term.is_empty() {

                    query.push(("xnxq", term.as_str()));
                }

                let courses = self
                    .pjxt_get(
                        "/evaluationMethodSix/getRequiredReviewsData",
                        &query,
                        "获取待评教课程",
                    )
                    .await?;

                for row in courses.as_array().into_iter().flatten() {

                    let Some(task) = parse_course(row, &rwid, &wjid, &msid) else {

                        continue;
                    };

                    match tasks.iter_mut().find(|known| known.id == task.id) {
                        Some(known) => known.evaluated |= task.evaluated,
                        None => tasks.push(task),
                    }
                }
            }
        }

        tasks.sort_by_key(|task| task.evaluated);

        Ok(tasks)
    }

    /// Loads a course's questionnaire.

    pub async fn evaluation_questionnaire(&self, task: &EvaluationTask) -> Result<Questionnaire> {

        self.evaluation_activate().await?;

        let query = topic_query(task);

        let query: Vec<(&str, &str)> = query
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();

        let value = self
            .pjxt_get(
                "/evaluationMethodSix/getQuestionnaireTopic",
                &query,
                "获取评教问卷",
            )
            .await?;

        // The questionnaire arrives as a one-element list.
        let topic = match value {
            Value::Array(mut items) => {

                if items.len() != 1 {

                    bail!(
                        "预期一份课程问卷，服务返回 {} 份，请到官网查看",
                        items.len()
                    );
                }

                items.remove(0)
            }
            other => other,
        };

        Ok(parse_questionnaire(&topic))
    }

    /// Submits the evaluation for one course.
    ///
    /// Why:
    /// This permanently records the user's answers. The caller must have
    /// confirmed explicitly; see the CLI, which refuses without `--yes`.
    ///
    /// How:
    /// School and college questions are submitted separately. A successful
    /// response is confirmed by reading this course's completion count.

    pub async fn evaluation_submit(
        &self,
        task: &EvaluationTask,
        questionnaire: &Questionnaire,
        answers: &[(String, String)],
    ) -> Result<EvaluationOutcome> {

        if answers.is_empty() {

            bail!("问卷没有可作答的题目，无法提交");
        }

        if questionnaire.parties.is_empty() {

            bail!("问卷没有返回评价对象，无法提交");
        }

        if let Some(question) = questionnaire.questions.iter().find(|question| {

            question.choice
                && question.required
                && !answers.iter().any(|(id, _)| *id == question.id)
        }) {

            bail!("必答题未作答: {}", question.text);
        }

        build_payload(task, questionnaire, answers)?;

        self.evaluation_activate().await?;

        self.evaluation_prepare_pattern(task).await?;

        // Selecting a pattern can allocate new result IDs. Retain the user's
        // previewed answers, but send the current server-issued templates.
        let current_questionnaire = self.evaluation_questionnaire(task).await?;

        if current_questionnaire.questions != questionnaire.questions {

            bail!("问卷题目或选项已变化，请重新预览并确认提交");
        }

        let results = build_payload(task, &current_questionnaire, answers)?;

        let response = self
            .client
            .post(pjxt_url(
                self.use_vpn,
                &format!("{PJXT_BASE}/evaluationMethodSix/submitSaveEvaluation"),
            ))
            .header("Content-Type", "application/json")
            .header("X-Requested-With", "XMLHttpRequest")
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
            .context("评教请求未取得响应，结果未知，请刷新核对后再操作")?
            .error_for_status()
            .context("评教服务返回 HTTP 错误，结果未知，请刷新核对后再操作")?;

        let body = response.text().await.context("读取评教提交响应失败")?;

        // Never resend a write, including when the response says "你已评价".
        // The same readback distinguishes a completed course from a conflict.
        let accepted = unwrap_envelope(&body);

        let term = text_field(&task.context, &["xnxq"]).unwrap_or_default();

        let mut query = vec![("wjid", task.wjid.as_str())];

        if !term.is_empty() {

            query.push(("xnxq", term.as_str()));
        }

        let current = self
            .pjxt_get(
                "/evaluationMethodSix/getRequiredReviewsData",
                &query,
                "回查评教完成状态",
            )
            .await;

        Ok(verified_outcome(task, accepted, current))
    }
}

fn pattern_needs_confirmation(questionnaires: &Value, wjid: &str) -> Result<bool> {

    let questionnaire = questionnaires
        .as_array()
        .context("评教问卷列表格式错误")?
        .iter()
        .find(|row| text_field(row, &["wjid"]).as_deref() == Some(wjid))
        .context("评教问卷已不在任务列表中，请刷新")?;

    match text_field(questionnaire, &["msid"]).as_deref() {
        None => Ok(true),
        Some("1") => Ok(false),
        Some("2") => bail!("问卷已选择评教模式二，请到官网完成；切换模式会丢失已有满意度数据"),
        Some(mode) => bail!("不支持的评教模式 {mode}，请到官网查看"),
    }
}

fn verified_outcome(
    task: &EvaluationTask,
    accepted: Result<Value>,
    current: Result<Value>,
) -> EvaluationOutcome {

    let status = current.and_then(|value| {

        let rows = value.as_array().context("课程列表格式错误")?;

        rows.iter()
            .filter_map(|row| parse_course(row, &task.rwid, &task.wjid, &task.msid))
            .find(|course| course.id == task.id)
            .map(|course| course.evaluated)
            .context("回查列表未找到这门课")
    });

    let rejection = accepted
        .as_ref()
        .err()
        .map(|error| format!("服务返回：{error}；"))
        .unwrap_or_default();

    let (success, message) = match status {
        Ok(true) => (true, format!("{rejection}已回查确认这门课评教完成")),
        Ok(false) => {
            (
                false,
                format!("{rejection}回查仍为未完成，未确认评教成功；请到官网核对后再操作"),
            )
        }
        Err(error) => {
            (
                false,
                format!(
                    "{rejection}无法确认评教完成：{error:#}；请刷新或到官网核对，勿直接重复提交"
                ),
            )
        }
    };

    EvaluationOutcome {
        course: task.course.clone(),
        success,
        message,
    }
}

/// Query for the questionnaire endpoint, built from the course row.
///
/// Why:
/// The service resolves the questionnaire from these fields and answers
/// "操作失败" when they are missing. Defaults are the values the web client
/// sends when a field is absent.

fn topic_query(task: &EvaluationTask) -> Vec<(&'static str, String)> {

    let field = |key: &str, default: &str| {

        text_field(&task.context, &[key]).unwrap_or_else(|| default.to_string())
    };

    vec![
        ("id", String::new()),
        ("rwid", task.rwid.clone()),
        ("wjid", task.wjid.clone()),
        ("zdmc", field("zdmc", "STID")),
        ("ypjcs", field("ypjcs", "0")),
        ("xypjcs", field("xypjcs", "1")),
        ("sxz", field("sxz", "")),
        ("pjrdm", field("pjrdm", "")),
        ("pjrmc", field("pjrmc", "")),
        ("bpdm", field("bpdm", "")),
        ("bpmc", field("bpmc", "")),
        ("kcdm", field("kcdm", &task.course_code)),
        ("kcmc", field("kcmc", &task.course)),
        ("rwh", field("rwh", "")),
        ("xn", field("xn", "")),
        ("xq", field("xq", "")),
        ("xnxq", field("xnxq", "")),
        ("pjlxid", field("pjlxid", "2")),
        ("sfksqbpj", field("sfksqbpj", "1")),
        ("yxsfktjst", field("yxsfktjst", "")),
        ("yxdm", field("ctyxdm", "")),
    ]
}

/// Builds the result objects sent to the submission endpoint.
///
/// Why:
/// The official topic page emits `wjzblist` with template 1, and `yxwjzblist`
/// with template 2 only when that source has questions.

fn build_payload(
    task: &EvaluationTask,
    questionnaire: &Questionnaire,
    answers: &[(String, String)],
) -> Result<Vec<Value>> {

    if let Some(reason) = &questionnaire.unsupported {

        bail!("{reason}，请到官网完成评教");
    }

    if questionnaire.questions.is_empty() {

        bail!("问卷没有题目，无法提交");
    }

    let mut payload = Vec::new();

    for source in [QuestionSource::School, QuestionSource::College] {

        let questions: Vec<_> = questionnaire
            .questions
            .iter()
            .filter(|question| question.source == source)
            .collect();

        if questions.is_empty() {

            continue;
        }

        let parties: Vec<_> = questionnaire
            .parties
            .iter()
            .filter(|party| text_field(party, &["sfxxpj"]).as_deref() == Some(source.code()))
            .collect();

        if parties.len() != 1 {

            bail!(
                "题目来源 {} 的评价记录数量异常：{}，无法提交",
                source.code(),
                parties.len()
            );
        }

        let party = parties[0];

        let mut total = 0.0;

        for question in &questions {

            let answer = answers.iter().find(|(id, _)| *id == question.id);

            if let Some((_, option_id)) = answer {

                let option = question
                    .options
                    .iter()
                    .find(|option| option.id == *option_id)
                    .with_context(|| format!("题目选项无效：{}", question.text))?;

                if source != QuestionSource::School
                    || !questionnaire
                        .unscored_school_questions
                        .contains(&question.id)
                {

                    total += option
                        .score
                        .with_context(|| format!("题目缺少选项分值：{}", question.text))?;
                }
            } else if question.required {

                bail!("必答题未作答：{}", question.text);
            }
        }

        if source == QuestionSource::School
            && let Some(divisor) = questionnaire.school_score_divisor
        {

            total = (total / divisor.max(1) as f64 * 100.0).round() / 100.0;
        }

        if questionnaire.full_score_at_95 && total >= 95.0 {

            total = 100.0;
        }

        let score = if total.fract() == 0.0 {

            json!(total as i64)
        } else {

            json!(total)
        };

        let field = |key: &str| party.get(key).cloned().unwrap_or(Value::Null);

        let round = party
            .get("wjssrwid")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| json!(task.rwid));

        let items: Vec<Value> = questions
            .iter()
            .map(|question| {

                let chosen = answers
                    .iter()
                    .find(|(id, _)| *id == question.id)
                    .map(|(_, option)| option.clone());

                if question.choice {

                    json!({
                        "sjly": source.code(),
                        "stlx": "1",
                        "wjid": task.wjid,
                        "wjssrwid": round,
                        "wjstctid": "",
                        "wjstid": question.id,
                        "xxdalist": chosen.into_iter().collect::<Vec<_>>(),
                    })
                } else {

                    json!({
                        "sjly": source.code(),
                        "stlx": "6",
                        "wjid": task.wjid,
                        "wjssrwid": round,
                        "wjstctid": question
                            .options
                            .first()
                            .map(|option| option.id.clone())
                            .unwrap_or_default(),
                        "wjstid": question.id,
                        "xxdalist": [],
                    })
                }
            })
            .collect();

        let teacher_key = party
            .get("pjrjsdm")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| json!(""));

        payload.push(json!({
                "bprdm": field("bprdm"),
                "bprmc": field("bprmc"),
                "kcdm": field("kcdm"),
                "kcmc": field("kcmc"),
                "pjdf": score,
                "pjfs": party.get("pjfs").filter(|value| !value.is_null()).cloned().unwrap_or_else(|| json!("1")),
                "pjid": field("pjid"),
                "pjlx": field("pjlx"),
                "pjmap": questionnaire.storage,
                "pjrdm": field("pjrdm"),
                "pjrjsdm": field("pjrjsdm"),
                "pjrxm": field("pjrxm"),
                "pjsx": 1,
                "pjxxlist": items,
                "rwh": field("rwh"),
                "stzjid": field("stzjid"),
                "wjid": task.wjid,
                "wjssrwid": round,
                "wtjjy": "",
                "xhgs": field("xhgs"),
                "xnxq": field("xnxq"),
                "sfxxpj": source.code(),
                "sqzt": field("sqzt"),
                "yxfz": field("yxfz"),
                "sdrs": field("sdrs"),
                "zsxz": teacher_key,
                "sfnm": "1",
            }));
    }

    Ok(payload)
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

    // This service's official pages accept only code 200, unlike other campus
    // APIs where 0 or 1 can mean success. Missing/unknown codes are not success.
    if text_field(&value, &["code"]).as_deref() != Some("200") {

        let message = value
            .get("message")
            .or_else(|| value.get("msg"))
            .and_then(Value::as_str)
            .unwrap_or("评教服务返回错误");

        bail!(
            "{message} (code={})",
            value.get("code").unwrap_or(&Value::Null)
        );
    }

    Ok(value.get("result").cloned().unwrap_or(value))
}

/// Parses one course row of `getRequiredReviewsData`.
///
/// How:
/// The row's own `rwid`/`wjid` win; the ids it was requested with fill in when
/// the row omits them. A row without a course or teacher code is still kept,
/// since the id stays unique within its questionnaire.

fn parse_course(row: &Value, rwid: &str, wjid: &str, msid: &str) -> Option<EvaluationTask> {

    let rwid = text_field(row, &["rwid"]).unwrap_or_else(|| rwid.to_string());

    let wjid = text_field(row, &["wjid"]).unwrap_or_else(|| wjid.to_string());

    if rwid.is_empty() || wjid.is_empty() {

        return None;
    }

    let course_code = text_field(row, &["kcdm"]).unwrap_or_default();

    let teacher_code = text_field(row, &["bpdm"]).unwrap_or_default();

    // `ypjcs` counts submitted evaluations, `xypjcs` the number required.
    let done = number_field(row, "ypjcs").unwrap_or(0);

    let required = number_field(row, "xypjcs").unwrap_or(1).max(1);

    Some(EvaluationTask {
        id: format!("{rwid}:{wjid}:{course_code}:{teacher_code}"),
        msid: text_field(row, &["msid"]).unwrap_or_else(|| msid.to_string()),
        course: text_field(row, &["kcmc"]).unwrap_or_else(|| "未知课程".to_string()),
        teacher: text_field(row, &["bpmc"]).unwrap_or_default(),
        evaluated: done >= required,
        context: row.clone(),
        rwid,
        wjid,
        course_code,
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

    let rows: Vec<&Value> = match entity.get("wjzblist").and_then(Value::as_array) {
        Some(sections) => {
            sections
                .iter()
                .filter_map(|section| section.get("tklist").and_then(Value::as_array))
                .flatten()
                .collect()
        }
        None => {
            entity
                .get("tklist")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .collect()
        }
    };

    let college_rows: Vec<_> = entity
        .get("yxwjzblist")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|section| section.get("tklist").and_then(Value::as_array))
        .flatten()
        .collect();

    let unsupported_question = rows
        .iter()
        .chain(&college_rows)
        .any(|row| !matches!(text_field(row, &["tmlx"]).as_deref(), Some("1" | "6")));

    let mut questions: Vec<_> = rows
        .into_iter()
        .filter_map(|row| parse_question(row, QuestionSource::School))
        .collect();

    questions.extend(
        college_rows
            .into_iter()
            .filter_map(|row| parse_question(row, QuestionSource::College)),
    );

    let sections: Vec<_> = entity
        .get("wjzblist")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();

    let unscored_school_questions = sections
        .iter()
        .filter(|section| text_field(section, &["sfcyjs"]).as_deref() == Some("0"))
        .filter_map(|section| section.get("tklist").and_then(Value::as_array))
        .flatten()
        .filter_map(|row| text_field(row, &["tmid"]))
        .collect();

    let school_score_divisor =
        (text_field(entity, &["sfatmsf"]).as_deref() == Some("1")).then(|| {

            sections
                .iter()
                .filter(|section| text_field(section, &["sfcyjs"]).as_deref() == Some("1"))
                .count()
                .max(1)
        });

    let unsupported = if unsupported_question {

        Some("问卷包含暂不支持的题型".into())
    } else if text_field(entity, &["wjfs"]).is_some_and(|kind| kind != "1") {

        Some("暂不支持该问卷的评分方式".into())
    } else if text_field(entity, &["sfyyxf"]).as_deref() == Some("1") {

        Some("问卷要求额外填写印象分".into())
    } else {

        None
    };

    Questionnaire {
        questions,
        parties: value
            .get("pjxtPjjgPjjgckb")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        storage: value.get("pjmap").cloned().unwrap_or(Value::Null),
        unscored_school_questions,
        school_score_divisor,
        full_score_at_95: text_field(entity, &["sfqyzgf"]).as_deref() == Some("1"),
        unsupported,
    }
}

fn parse_question(row: &Value, source: QuestionSource) -> Option<Question> {

    let id = text_field(row, &["tmid"])?;

    // `tmlx` is "1" for single choice; "6" is free text, whose one option is
    // only a slot for the typed answer.
    let kind = text_field(row, &["tmlx"]).unwrap_or_default();

    let options = row
        .get("tmxxlist")
        .and_then(Value::as_array)
        .map(|rows| {

            rows.iter()
                .filter_map(|option| {

                    Some(QuestionOption {
                        id:    text_field(option, &["tmxxid"])?,
                        label: text_field(option, &["xxmc", "tmxxmc"]).unwrap_or_default(),
                        score: option.get("xxfz").and_then(|value| {

                            value
                                .as_f64()
                                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
                        }),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Some(Question {
        id,
        text: text_field(row, &["tgmc", "tmmc", "tmnr"]).unwrap_or_default(),
        choice: kind == "1",
        required: text_field(row, &["sfbd"]).is_some_and(|value| value == "1"),
        options,
        source,
    })
}

/// First non-empty text among `keys`, accepting numbers as text.

fn text_field(row: &Value, keys: &[&str]) -> Option<String> {

    keys.iter()
        .filter_map(|key| row.get(*key))
        .filter_map(|value| {

            match value {
                Value::String(text) => Some(text.trim().to_string()),
                Value::Number(number) => Some(number.to_string()),
                _ => None,
            }
        })
        .find(|text| !text.is_empty())
}

fn number_field(row: &Value, key: &str) -> Option<i64> {

    let value = row.get(key)?;

    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

#[cfg(test)]

mod tests {

    use super::{
        EvaluationTask, Question, QuestionOption, Questionnaire, build_payload, parse_course,
        parse_questionnaire, pattern_needs_confirmation, topic_query, unwrap_envelope,
        verified_outcome,
    };
    use serde_json::{Value, json};

    /// A course row as `getRequiredReviewsData` returned it (trimmed).

    fn course_row() -> Value {

        json!({
            "bpdm": "11436",
            "bpmc": "石琳",
            "kcdm": "B210031011",
            "kcmc": "软件工程综合实践",
            "pjlxid": "2",
            "pjrdm": "23371544",
            "pjrmc": "张义",
            "rwh": "202520263B210031011003",
            "rwid": "r1",
            "sfksqbpj": "1",
            "sxz": "SXZ",
            "wjid": "w1",
            "xn": null,
            "xnxq": "2025-20263",
            "xq": null,
            "xypjcs": 1,
            "ypjcs": 0,
            "yxsfktjst": null,
            "zdmc": "STID"
        })
    }

    fn option(id: &str, label: &str, score: f64) -> Value {

        json!({"tmxxid": id, "xxmc": label, "xxfz": score})
    }

    /// A questionnaire as `getQuestionnaireTopic` returned it (trimmed).

    fn topic() -> Value {

        let party = |pjid: &str, sfxxpj: &str| {

            json!({
                "bprdm": "11436", "bprmc": "石琳", "kcdm": "B210031011",
                "kcmc": "软件工程综合实践", "pjfs": "1", "pjid": pjid, "pjlx": "2",
                "pjrdm": "23371544", "pjrjsdm": "SXZ", "pjrxm": "张义",
                "rwh": "202520263B210031011003", "sfxxpj": sfxxpj, "wjid": "w1",
                "wjssrwid": "r1", "xnxq": "2025-20263"
            })
        };

        json!([{
            "pjmap": {"PJJGBM": "A", "PJJGXXBM": "B", "RWID": "r1"},
            "pjxtPjjgPjjgckb": [party("p1", "1"), party("p2", "2")],
            "pjxtWjWjbReturnEntity": {
                "yxwjzblist": [],
                "wjzblist": [{
                    "zmc": "问卷1",
                    "tklist": [
                        {"tmid": "1", "tmlx": "1", "sfbd": "1", "tgmc": "授课热情投入", "tmmc": null,
                         "tmxxlist": [option("11", "优秀", 9.5), option("12", "良好", 7.5)]},
                        {"tmid": "2", "tmlx": "1", "sfbd": "1", "tgmc": "要求明确",
                         "tmxxlist": [option("21", "优秀", 9.5), option("22", "良好", 7.5)]},
                        {"tmid": "3", "tmlx": "1", "sfbd": "1", "tgmc": "总体评价",
                         "tmxxlist": [option("31", "优秀", 47.5), option("32", "良好", 37.5)]},
                        {"tmid": "4", "tmlx": "6", "sfbd": "0", "tgmc": "优秀之处",
                         "tmxxlist": [{"tmxxid": "41", "xxmc": null, "xxfz": 0}]}
                    ]
                }]
            }
        }])
    }

    fn task() -> EvaluationTask {

        parse_course(&course_row(), "r1", "w1", "1").expect("应能解析课程")
    }

    fn questionnaire() -> Questionnaire {

        parse_questionnaire(&topic()[0])
    }

    #[test]

    fn a_course_row_becomes_a_task_with_a_unique_id() {

        let task = task();

        assert_eq!(task.id, "r1:w1:B210031011:11436");

        assert_eq!(task.course, "软件工程综合实践");

        assert_eq!(task.teacher, "石琳");

        assert_eq!(task.msid, "1");

        assert!(!task.evaluated, "ypjcs=0 表示未评");

        let mut done = course_row();

        done["ypjcs"] = json!(1);

        assert!(parse_course(&done, "r1", "w1", "1").unwrap().evaluated);
    }

    #[test]

    fn the_course_row_is_kept_but_not_printed() {

        let task = task();

        assert_eq!(task.context["sxz"], json!("SXZ"));

        let printed = serde_json::to_value(&task).unwrap();

        assert!(printed.get("context").is_none(), "{printed}");
    }

    #[test]

    fn the_topic_query_carries_the_course_fields() {

        let query = topic_query(&task());

        let get = |key: &str| {

            query
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.as_str())
        };

        assert_eq!(get("sxz"), Some("SXZ"));

        assert_eq!(get("bpdm"), Some("11436"));

        assert_eq!(get("rwh"), Some("202520263B210031011003"));

        assert_eq!(get("ypjcs"), Some("0"));

        assert_eq!(get("xn"), Some(""), "null 字段发空串");

        assert_eq!(get("id"), Some(""));
    }

    #[test]

    fn parses_the_questionnaire_the_service_returns() {

        let questionnaire = questionnaire();

        assert_eq!(questionnaire.questions.len(), 4);

        let first = &questionnaire.questions[0];

        assert_eq!(first.text, "授课热情投入", "题干在 tgmc 中");

        assert!(first.choice && first.required);

        assert_eq!(first.options[1].label, "良好");

        assert_eq!(first.options[1].score, Some(7.5));

        assert!(!questionnaire.questions[3].choice);

        assert_eq!(questionnaire.parties.len(), 2);

        assert_eq!(questionnaire.storage["RWID"], json!("r1"));
    }

    #[test]

    fn default_answers_vary_exactly_one_choice() {

        let answers = questionnaire().default_answers();

        let pairs: Vec<(&str, &str)> = answers
            .iter()
            .map(|(question, option)| (question.as_str(), option.as_str()))
            .collect();

        assert_eq!(pairs, vec![("1", "12"), ("2", "21"), ("3", "31")]);
    }

    #[test]

    fn a_choice_question_with_no_options_is_not_answered() {

        let questionnaire = Questionnaire {
            questions: vec![Question {
                id: "q".to_string(),
                choice: true,
                ..Default::default()
            }],
            ..Default::default()
        };

        assert!(questionnaire.default_answers().is_empty());
    }

    #[test]

    fn school_only_questionnaire_does_not_submit_the_empty_college_record() {

        let questionnaire = questionnaire();

        let answers = questionnaire.default_answers();

        let payload = build_payload(&task(), &questionnaire, &answers).unwrap();

        assert_eq!(
            payload.len(),
            1,
            "官网只提交有题目的来源，学院模板不代表第二个评价对象"
        );

        assert_eq!(payload[0]["pjid"], json!("p1"));

        assert_eq!(payload[0]["sfxxpj"], json!("1"));

        assert_eq!(payload[0]["pjmap"]["PJJGBM"], json!("A"));

        // 7.5 + 9.5 + 47.5
        assert_eq!(payload[0]["pjdf"], json!(64.5));

        let items = payload[0]["pjxxlist"].as_array().unwrap();

        assert_eq!(items.len(), 4, "主观题也要带上空答案");

        assert_eq!(items[0]["xxdalist"], json!(["12"]));

        assert_eq!(items[3]["stlx"], json!("6"));

        assert_eq!(items[3]["wjstctid"], json!("41"));

        assert_eq!(items[3]["xxdalist"], json!([]));
    }

    #[test]

    fn school_and_college_answers_are_not_copied_between_sources() {

        let mut value = topic()[0].clone();

        value["pjxtWjWjbReturnEntity"]["yxwjzblist"] = json!([{
            "tklist": [{"tmid": "college-q", "tmlx": "1", "sfbd": "1", "tgmc": "学院题目",
                "tmxxlist": [option("college-o", "优秀", 20.0)]}]
        }]);

        let questionnaire = parse_questionnaire(&value);

        assert_eq!(questionnaire.questions.len(), 5);

        let payload =
            build_payload(&task(), &questionnaire, &questionnaire.default_answers()).unwrap();

        assert_eq!(payload.len(), 2);

        assert_eq!(payload[0]["pjxxlist"].as_array().unwrap().len(), 4);

        assert_eq!(payload[1]["pjxxlist"].as_array().unwrap().len(), 1);

        assert_eq!(payload[1]["pjid"], json!("p2"));

        assert_eq!(payload[1]["sfxxpj"], json!("2"));

        assert_eq!(payload[1]["pjxxlist"][0]["sjly"], json!("2"));

        assert_eq!(payload[1]["pjxxlist"][0]["wjstid"], json!("college-q"));

        assert_eq!(payload[1]["pjdf"], json!(20));

        assert_eq!(payload[0]["pjdf"], json!(64.5));
    }

    #[test]

    fn whole_scores_are_sent_as_integers() {

        let questionnaire = Questionnaire {
            questions: vec![Question {
                id: "q".to_string(),
                choice: true,
                options: vec![QuestionOption {
                    id:    "o".to_string(),
                    label: "优秀".to_string(),
                    score: Some(93.0),
                }],
                ..Default::default()
            }],
            parties: vec![json!({"pjid": "p", "sfxxpj": "1"})],
            ..Default::default()
        };

        let payload =
            build_payload(&task(), &questionnaire, &questionnaire.default_answers()).unwrap();

        assert_eq!(payload[0]["pjdf"], json!(93));
    }

    #[test]

    fn option_labels_are_shown_instead_of_ids() {

        let questionnaire = questionnaire();

        assert_eq!(questionnaire.questions[0].option_label("12"), "良好");

        assert_eq!(questionnaire.questions[0].option_label("??"), "??");
    }

    #[test]

    fn envelope_failures_are_surfaced() {

        assert!(unwrap_envelope(r#"{"code":200,"result":{"a":1}}"#).is_ok());

        let error = unwrap_envelope(r#"{"code":500,"message":"服务器错误"}"#)
            .unwrap_err()
            .to_string();

        assert!(error.contains("服务器错误"), "应保留服务端消息: {error}");

        let error = unwrap_envelope(r#"{"code":"500","msg":"操作失败"}"#)
            .unwrap_err()
            .to_string();

        assert!(error.contains("操作失败"), "{error}");
    }

    #[test]

    fn envelope_result_is_unwrapped() {

        let value = unwrap_envelope(r#"{"code":"200","result":{"list":[1,2]}}"#).expect("应能解析");

        assert_eq!(value["list"].as_array().map(Vec::len), Some(2));
    }

    #[test]

    fn unexpected_codes_never_claim_success() {

        for body in [
            r#"{"code":1,"msg":"你已评价"}"#,
            r#"{"code":0}"#,
            r#"{"code":null}"#,
            r#"{"code":true}"#,
            r#"{"result":{}}"#,
        ] {

            assert!(unwrap_envelope(body).is_err(), "{body}");
        }
    }

    #[test]

    fn first_pattern_is_confirmed_without_resetting_an_existing_pattern() {

        assert!(pattern_needs_confirmation(&json!([{"wjid":"w1", "msid":null}]), "w1").unwrap());

        assert!(!pattern_needs_confirmation(&json!([{"wjid":"w1", "msid":"1"}]), "w1").unwrap());

        assert!(
            pattern_needs_confirmation(&json!([{"wjid":"w1", "msid":"2"}]), "w1")
                .unwrap_err()
                .to_string()
                .contains("丢失")
        );

        assert!(pattern_needs_confirmation(&json!([]), "w1").is_err());
    }

    #[test]

    fn response_success_is_not_completion_when_the_course_is_still_pending() {

        let outcome = verified_outcome(&task(), Ok(json!({})), Ok(json!([course_row()])));

        assert!(!outcome.success);

        assert!(outcome.message.contains("回查仍为未完成"));

        let rejected = verified_outcome(&task(), Err(anyhow::anyhow!("你已评价")), Ok(json!([])));

        assert!(!rejected.success);
    }

    #[test]

    fn only_the_same_course_with_completed_count_confirms_submission() {

        let mut row = course_row();

        row["ypjcs"] = json!(1);

        assert!(verified_outcome(&task(), Ok(json!({})), Ok(json!([row.clone()]))).success);

        assert!(
            verified_outcome(
                &task(),
                Err(anyhow::anyhow!("你已评价")),
                Ok(json!([row.clone()]))
            )
            .success
        );

        row["bpdm"] = json!("someone-else");

        assert!(!verified_outcome(&task(), Ok(json!({})), Ok(json!([row]))).success);

        assert!(
            !verified_outcome(
                &task(),
                Ok(json!({})),
                Err(anyhow::anyhow!("network error"))
            )
            .success
        );
    }

    #[test]

    fn invalid_or_unscored_answers_do_not_produce_a_submission() {

        let mut questionnaire = questionnaire();

        assert!(build_payload(&task(), &questionnaire, &[("1".into(), "invalid".into())]).is_err());

        questionnaire.questions[0].options[1].score = None;

        assert!(build_payload(&task(), &questionnaire, &questionnaire.default_answers()).is_err());

        questionnaire.questions[0].options[1].score = Some(7.5);

        questionnaire.parties.clear();

        assert!(build_payload(&task(), &questionnaire, &questionnaire.default_answers()).is_err());
    }

    #[test]

    fn scoring_obeys_section_weights_and_the_sites_full_score_rule() {

        let mut value = topic()[0].clone();

        let entity = &mut value["pjxtWjWjbReturnEntity"];

        entity["sfatmsf"] = json!("1");

        entity["wjzblist"][0]["sfcyjs"] = json!("1");

        entity["wjzblist"].as_array_mut().unwrap().push(json!({
            "sfcyjs":"1", "tklist":[{"tmid":"extra", "tmlx":"1", "sfbd":"1",
                "tmxxlist":[option("extra-o", "优秀", 35.5)]}]
        }));

        entity["wjzblist"].as_array_mut().unwrap().push(json!({
            "sfcyjs":"0", "tklist":[{"tmid":"excluded", "tmlx":"1", "sfbd":"1",
                "tmxxlist":[option("excluded-o", "优秀", 100.0)]}]
        }));

        let questionnaire = parse_questionnaire(&value);

        let payload =
            build_payload(&task(), &questionnaire, &questionnaire.default_answers()).unwrap();

        assert_eq!(payload[0]["pjdf"], json!(50));

        assert_eq!(payload[0]["pjxxlist"].as_array().unwrap().len(), 6);

        value["pjxtWjWjbReturnEntity"]["sfatmsf"] = json!("0");

        value["pjxtWjWjbReturnEntity"]["sfqyzgf"] = json!("1");

        value["pjxtWjWjbReturnEntity"]["wjzblist"][1]["tklist"][0]["tmxxlist"][0]["xxfz"] =
            json!(31.0);

        let questionnaire = parse_questionnaire(&value);

        let payload =
            build_payload(&task(), &questionnaire, &questionnaire.default_answers()).unwrap();

        assert_eq!(payload[0]["pjdf"], json!(100));
    }

    #[test]

    fn unsupported_question_types_are_not_submitted_as_blank_free_text() {

        let mut value = topic()[0].clone();

        value["pjxtWjWjbReturnEntity"]["wjzblist"][0]["tklist"][0]["tmlx"] = json!("2");

        let questionnaire = parse_questionnaire(&value);

        let error =
            build_payload(&task(), &questionnaire, &questionnaire.default_answers()).unwrap_err();

        assert!(error.to_string().contains("题型"));
    }
}
