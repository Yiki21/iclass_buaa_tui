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

        let task = tasks
            .iter()
            .find(|task| task.rwid == task_id)
            .ok_or_else(|| anyhow::anyhow!("没有评教任务 {task_id}"))?;

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
                    "default_answers": questionnaire.default_answers(),
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

    println!("task\trwid\t课程\t教师\t状态");

    for task in &tasks {

        println!(
            "{}\t{}\t{}\t{}\t{}",
            index_of(&tasks, task),
            task.rwid,
            task.course,
            dash(&task.teacher),
            if task.evaluated { "已评" } else { "未评" },
        );
    }

    println!();

    println!("用 --show <rwid> 查看某门课的问卷，以及将会提交的答案。");

    Ok(())
}

pub(crate) async fn eval_submit_command(args: EvalSubmitArgs) -> Result<()> {

    if args.tasks.is_empty() && !args.all {

        bail!("请用 --task <rwid> 指定要评教的课程，或用 --all 评教全部未评课程");
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

            let task = tasks
                .iter()
                .find(|task| &task.rwid == id)
                .ok_or_else(|| anyhow::anyhow!("没有评教任务 {id}"))?;

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
                        "rwid": task.rwid,
                        "course": task.course,
                        "answers": questionnaire.default_answers(),
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

        match api.evaluation_submit(task, &answers).await {
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

/// Prints a questionnaire and the answers that would be submitted.

fn print_questionnaire(
    writer: &mut dyn Write,
    task: &EvaluationTask,
    questionnaire: &Questionnaire,
) -> Result<()> {

    // Writes through the caller's sink, so `--json` keeps stdout clean.
    writeln!(writer, "课程\t{}", task.course)?;

    writeln!(writer, "任务\t{}", task.rwid)?;

    if !questionnaire.questions.is_empty() {

        writeln!(writer)?;

        // Showing the questions is the point: it makes visible what is being
        // answered on the user's behalf.
        for (index, question) in questionnaire.questions.iter().enumerate() {

            let answer = question
                .options
                .first()
                .map(|option| format!("→ {option}"))
                .unwrap_or_else(|| "（不作答）".to_string());

            writeln!(writer, "\t{}. {}\t{}", index + 1, question.text, answer)?;
        }
    }

    Ok(())
}

/// Position of a task in the list, used as a short handle.

fn index_of(tasks: &[EvaluationTask], task: &EvaluationTask) -> usize {

    tasks
        .iter()
        .position(|candidate| candidate.rwid == task.rwid)
        .unwrap_or(0)
}

fn dash(value: &str) -> &str {

    if value.trim().is_empty() { "-" } else { value }
}

#[cfg(test)]

mod tests {

    use super::print_questionnaire;

    use crate::evaluation::{EvaluationTask, Question, Questionnaire};

    fn task() -> EvaluationTask {

        EvaluationTask {
            rwid:        "1".to_string(),
            wjid:        "2".to_string(),
            course:      "操作系统".to_string(),
            course_code: "B3".to_string(),
            teacher:     "王老师".to_string(),
            evaluated:   false,
        }
    }

    fn questionnaire() -> Questionnaire {

        Questionnaire {
            questions: vec![Question {
                id:      "q1".to_string(),
                text:    "课程内容如何？".to_string(),
                options: vec!["很好".to_string(), "一般".to_string()],
                choice:  true,
            }],
        }
    }

    #[test]

    fn the_questionnaire_writes_through_the_sink_it_is_given() {

        // The `--json` paths rely on this: prose must go wherever the caller
        // points them, so stdout can stay one JSON document.
        let mut sink: Vec<u8> = Vec::new();

        print_questionnaire(&mut sink, &task(), &questionnaire()).expect("写入问卷");

        let text = String::from_utf8(sink).expect("应该是 UTF-8");

        assert!(text.contains("操作系统"), "缺少课程名：{text}");

        assert!(text.contains("课程内容如何？"), "缺少题干：{text}");

        assert!(text.contains("→ 很好"), "缺少默认答案：{text}");
    }
}
