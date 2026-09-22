//! CLI commands for course evaluation (评教).
//!
//! Why:
//! Evaluation is mandatory before grades are released, so listing what is
//! outstanding is genuinely useful. Submitting, however, records answers in the
//! user's name without them reading the questions, and cannot be undone. The
//! two are therefore separate commands, and submission refuses to run without
//! an explicit confirmation flag.

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

        print_questionnaire(task, &questionnaire);

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

        println!("没有需要评教的课程");

        return Ok(());
    }

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

    println!("将提交以下评教：");

    println!();

    for (task, questionnaire) in &prepared {

        print_questionnaire(task, questionnaire);

        println!();
    }

    println!("注意：提交后无法撤销，且评教结果会记在你的名下，而你并未逐题作答。");

    if !args.yes {

        println!();

        println!("这是预览。确认无误后加上 --yes 才会真正提交。");

        return Ok(());
    }

    let mut succeeded = 0;

    let mut failures = Vec::new();

    for (task, questionnaire) in &prepared {

        let answers = questionnaire.default_answers();

        match api.evaluation_submit(task, &answers).await {
            Ok(outcome) if outcome.success => {

                println!("评教成功\t{}", task.course);

                succeeded += 1;
            }
            Ok(outcome) => {

                println!("评教失败\t{}\t{}", task.course, outcome.message);

                failures.push(format!("{}: {}", task.course, outcome.message));
            }
            Err(error) => {

                println!("评教失败\t{}\t{error}", task.course);

                failures.push(format!("{}: {error}", task.course));
            }
        }
    }

    println!();

    println!("完成：成功 {succeeded} 门，失败 {} 门", failures.len());

    if !failures.is_empty() {

        bail!("部分课程评教失败:\n{}", failures.join("\n"));
    }

    Ok(())
}

/// Prints a questionnaire and the answers that would be submitted.

fn print_questionnaire(task: &EvaluationTask, questionnaire: &Questionnaire) {

    println!("课程\t{}", task.course);

    println!("任务\t{}", task.rwid);

    if !questionnaire.questions.is_empty() {

        println!();

        // Showing the questions is the point: it makes visible what is being
        // answered on the user's behalf.
        for (index, question) in questionnaire.questions.iter().enumerate() {

            let answer = question
                .options
                .first()
                .map(|option| format!("→ {option}"))
                .unwrap_or_else(|| "（不作答）".to_string());

            println!("\t{}. {}\t{}", index + 1, question.text, answer);
        }
    }
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
