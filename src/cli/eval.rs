//! CLI commands for course evaluation (评教).
//!
//! Why:
//! Evaluation is mandatory before grades are released, so listing what is
//! outstanding is genuinely useful. Submitting, however, records answers in the
//! user's name without them reading the questions, and cannot be undone. The
//! two are therefore separate commands, and submission refuses to run without
//! an explicit confirmation flag.

use std::io::Write;

use anyhow::{Context, Result, bail};

use crate::evaluation::{EvaluationTask, Questionnaire};

use super::args::{EvalArgs, EvalSubmitArgs};
use super::config::load_config;
use super::venue::authenticated_api;

pub(crate) async fn eval_command(args: EvalArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let api = authenticated_api(&config, args.debug_login).await?;

    let user_id = config.vpn_username.trim().to_string();

    let user_id = if user_id.is_empty() {

        config.student_id.clone()
    } else {

        user_id
    };

    let tasks = api
        .evaluation_tasks(&user_id)
        .await
        .context("获取待评教列表失败")?;

    if let Some(task_id) = args.show.as_deref() {

        let task = find_task(&tasks, task_id)?;

        let questionnaire = api
            .evaluation_questionnaire(task)
            .await
            .context("获取问卷失败")?;

        if args.json {

            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "task": task,
                    "questionnaire": questionnaire,
                    "default_answers": answers_with_labels(&questionnaire),
                }))?
            );

            return Ok(());
        }

        // `--json` already returned above, so this is the human path.
        print_questionnaire(&mut std::io::stdout(), task, &questionnaire)?;

        return Ok(());
    }

    if args.json {

        println!("{}", serde_json::to_string_pretty(&tasks)?);

        return Ok(());
    }

    if tasks.is_empty() {

        println!("没有待评教课程");

        return Ok(());
    }

    let pending = tasks.iter().filter(|task| !task.evaluated).count();

    println!(
        "待评教\t{} 门（已评 {} 门）",
        pending,
        tasks.len() - pending
    );

    println!();

    println!("状态\t课程\t教师\ttask");

    for task in &tasks {

        println!(
            "{}\t{}\t{}\t{}",
            if task.evaluated { "已评" } else { "未评" },
            task.course,
            dash(&task.teacher),
            task.id,
        );
    }

    println!();

    println!("用 --show <task> 查看某门课的问卷，以及将会提交的答案。");

    Ok(())
}

pub(crate) async fn eval_submit_command(args: EvalSubmitArgs) -> Result<()> {

    if args.tasks.is_empty() && !args.all {

        bail!("请用 --task <task> 指定要评教的课程，或用 --all 评教全部未评课程");
    }

    let config = load_config(args.config.as_deref())?;

    let api = authenticated_api(&config, args.debug_login).await?;

    let user_id = {

        let name = config.vpn_username.trim();

        if name.is_empty() {

            config.student_id.clone()
        } else {

            name.to_string()
        }
    };

    let tasks = api
        .evaluation_tasks(&user_id)
        .await
        .context("获取待评教列表失败")?;

    let selected: Vec<&EvaluationTask> = if args.all {

        tasks.iter().filter(|task| !task.evaluated).collect()
    } else {

        let mut chosen = Vec::new();

        for id in &args.tasks {

            let task = find_task(&tasks, id)?;

            chosen.push(task);
        }

        chosen
    };

    if selected.is_empty() {

        if args.json {

            // A caller that asked for JSON gets JSON, even when there is
            // nothing to submit: an empty stdout is not a document.
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "action": "eval-submit",
                    "submitted": false,
                    "would_submit": [],
                    "hint": "没有需要评教的课程",
                }))?
            );

            return Ok(());
        }

        println!("没有需要评教的课程");

        return Ok(());
    }

    // Under `--json` stdout carries exactly one document, so every human line
    // goes to stderr instead. Prose on stdout would make the output
    // unparseable for the callers this flag exists for.
    let mut human: Box<dyn Write> = if args.json {

        Box::new(std::io::stderr())
    } else {

        Box::new(std::io::stdout())
    };

    // Load every questionnaire first, so the preview is complete before any
    // submission happens.
    let mut prepared = Vec::new();

    for task in &selected {

        let questionnaire = api
            .evaluation_questionnaire(task)
            .await
            .with_context(|| format!("获取 {} 的问卷失败", task.course))?;

        prepared.push((*task, questionnaire));
    }

    writeln!(human, "将提交以下评教：")?;

    writeln!(human)?;

    for (task, questionnaire) in &prepared {

        print_questionnaire(&mut human, task, questionnaire)?;

        writeln!(human)?;
    }

    writeln!(
        human,
        "注意：提交后无法撤销，且评教结果会记在你的名下，而你并未逐题作答。"
    )?;

    if !args.yes {

        if args.json {

            let preview: Vec<serde_json::Value> = prepared
                .iter()
                .map(|(task, questionnaire)| {

                    serde_json::json!({
                        "task": task.id,
                        "course": task.course,
                        "answers": answers_with_labels(questionnaire),
                    })
                })
                .collect();

            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "action": "eval-submit",
                    "submitted": false,
                    "would_submit": preview,
                    "hint": "加上 --yes 才会真正提交；提交会以你的名义作答",
                }))?
            );

            return Ok(());
        }

        writeln!(human)?;

        writeln!(human, "这是预览。确认无误后加上 --yes 才会真正提交。")?;

        return Ok(());
    }

    let mut succeeded = 0;

    let mut failures = Vec::new();

    for (task, questionnaire) in &prepared {

        let answers = questionnaire.default_answers();

        match api.evaluation_submit(task, questionnaire, &answers).await {
            Ok(outcome) if outcome.success => {

                writeln!(human, "评教成功\t{}", task.course)?;

                succeeded += 1;
            }
            Ok(outcome) => {

                writeln!(human, "评教失败\t{}\t{}", task.course, outcome.message)?;

                failures.push(format!("{}: {}", task.course, outcome.message));
            }
            Err(error) => {

                writeln!(human, "评教失败\t{}\t{error}", task.course)?;

                failures.push(format!("{}: {error}", task.course));
            }
        }
    }

    if args.json {

        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "action": "eval-submit",
                "submitted": true,
                "succeeded": succeeded,
                "failures": failures,
            }))?
        );

        if !failures.is_empty() {

            bail!("部分课程评教失败",);
        }

        return Ok(());
    }

    writeln!(human)?;

    writeln!(
        human,
        "完成：成功 {succeeded} 门，失败 {} 门",
        failures.len()
    )?;

    if !failures.is_empty() {

        bail!("部分课程评教失败:\n{}", failures.join("\n"));
    }

    Ok(())
}

/// Finds the task a user named on the command line.
///
/// Why:
/// The task id is the only unique handle: one evaluation round covers every
/// course of the term, so a round id (`rwid`) alone cannot name a course.
///
/// How:
/// Accepts the full task id, or a round id when exactly one course uses it.

fn find_task<'a>(tasks: &'a [EvaluationTask], wanted: &str) -> Result<&'a EvaluationTask> {

    if let Some(task) = tasks.iter().find(|task| task.id == wanted) {

        return Ok(task);
    }

    let matches: Vec<&EvaluationTask> = tasks.iter().filter(|task| task.rwid == wanted).collect();

    match matches.as_slice() {
        [task] => Ok(task),
        [] => bail!("没有评教任务 {wanted}"),
        _ => {

            bail!(
                "{wanted} 是评教轮次，对应 {} 门课程，请用 task 列中的完整 id：{}",
                matches.len(),
                matches
                    .iter()
                    .map(|task| task.id.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }
}

/// The answers that would be sent, with the option text next to each id.
///
/// Why:
/// An id alone does not tell the user what they are about to answer.

fn answers_with_labels(questionnaire: &Questionnaire) -> Vec<serde_json::Value> {

    questionnaire
        .default_answers()
        .into_iter()
        .map(|(question_id, option_id)| {

            let question = questionnaire
                .questions
                .iter()
                .find(|candidate| candidate.id == question_id);

            serde_json::json!({
                "question": question_id,
                "text": question.map(|question| question.text.clone()),
                "option": option_id,
                "label": question.map(|question| question.option_label(&option_id)),
            })
        })
        .collect()
}

/// Prints a questionnaire and the answers that would be submitted.

fn print_questionnaire(
    writer: &mut dyn Write,
    task: &EvaluationTask,
    questionnaire: &Questionnaire,
) -> Result<()> {

    // Writes through the caller's sink, so `--json` keeps stdout clean.
    writeln!(writer, "课程\t{}", task.course)?;

    writeln!(writer, "教师\t{}", dash(&task.teacher))?;

    writeln!(writer, "任务\t{}", task.id)?;

    if !questionnaire.questions.is_empty() {

        writeln!(writer)?;

        // Showing the questions is the point: it makes visible what is being
        // answered on the user's behalf.
        let answers = questionnaire.default_answers();

        for (index, question) in questionnaire.questions.iter().enumerate() {

            let answer = answers
                .iter()
                .find(|(id, _)| *id == question.id)
                .map(|(_, option)| format!("→ {}", question.option_label(option)))
                .unwrap_or_else(|| "（不作答）".to_string());

            writeln!(writer, "\t{}. {}\t{}", index + 1, question.text, answer)?;
        }
    }

    Ok(())
}

fn dash(value: &str) -> &str {

    if value.trim().is_empty() { "-" } else { value }
}

#[cfg(test)]

mod tests {

    use super::{find_task, print_questionnaire};

    use crate::evaluation::{EvaluationTask, Question, QuestionOption, Questionnaire};

    fn task(rwid: &str, course_code: &str) -> EvaluationTask {

        EvaluationTask {
            id: format!("{rwid}:w:{course_code}:t"),
            rwid: rwid.to_string(),
            wjid: "w".to_string(),
            course: "操作系统".to_string(),
            course_code: course_code.to_string(),
            teacher: "王老师".to_string(),
            ..Default::default()
        }
    }

    fn choice(id: &str, text: &str) -> Question {

        let option = |id: &str, label: &str| {

            QuestionOption {
                id:    id.to_string(),
                label: label.to_string(),
                score: None,
            }
        };

        Question {
            id:       id.to_string(),
            text:     text.to_string(),
            options:  vec![option("o1", "很好"), option("o2", "一般")],
            choice:   true,
            required: true,
        }
    }

    fn questionnaire() -> Questionnaire {

        Questionnaire {
            questions: vec![choice("q1", "课程内容如何？"), choice("q2", "总体评价")],
            ..Default::default()
        }
    }

    #[test]

    fn the_questionnaire_writes_through_the_sink_it_is_given() {

        // The `--json` paths rely on this: prose must go wherever the caller
        // points them, so stdout can stay one JSON document.
        let mut sink: Vec<u8> = Vec::new();

        print_questionnaire(&mut sink, &task("r", "B3"), &questionnaire()).expect("写入问卷");

        let text = String::from_utf8(sink).expect("应该是 UTF-8");

        assert!(text.contains("操作系统"), "缺少课程名：{text}");

        assert!(text.contains("课程内容如何？"), "缺少题干：{text}");

        // The printed answers are the ones submitted, shown by label.
        assert!(text.contains("课程内容如何？\t→ 一般"), "{text}");

        assert!(text.contains("总体评价\t→ 很好"), "{text}");
    }

    #[test]

    fn a_round_id_names_a_course_only_when_it_is_unambiguous() {

        let one = vec![task("r", "B3")];

        assert_eq!(find_task(&one, "r").unwrap().course_code, "B3");

        assert_eq!(find_task(&one, "r:w:B3:t").unwrap().course_code, "B3");

        let two = vec![task("r", "B3"), task("r", "B4")];

        let error = find_task(&two, "r").unwrap_err().to_string();

        assert!(error.contains("r:w:B4:t"), "应列出可选 id: {error}");

        assert_eq!(find_task(&two, "r:w:B4:t").unwrap().course_code, "B4");

        assert!(find_task(&two, "nope").is_err());
    }
}
