//! CLI commands for BYKC (博雅课程) browsing and enrollment.
//!
//! Why:
//! The TUI could browse, select, and withdraw BYKC courses while the CLI could
//! only sign. An automated caller had no way to see what is open or to enroll,
//! so these commands give the CLI the same reach. Enrollment and withdrawal
//! follow the tool's write rule: preview by default, act only with `--yes`.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;

use crate::bykc::{BykcApi, BykcCourseDetail};
use crate::iclass::IClassApi;

use super::args::{BykcCourseArgs, BykcCoursesArgs, BykcReadArgs, BykcWriteArgs};
use super::config::load_config;

/// Logs in and returns the BYKC client that rides on the SSO session.
///
/// Why:
/// BYKC is only reachable through WebVPN, and its client must reuse the SSO
/// cookies of the main login (see `BykcApi::with_cookie_jar`). Failing early
/// with the configuration fix is clearer than an opaque CAS error.

async fn bykc_api(args: &BykcReadArgs) -> Result<BykcApi> {

    let config = load_config(args.config.as_deref())?;

    if !config.use_vpn {

        bail!("博雅课程需要 VPN 模式：在配置文件中设置 use_vpn = true");
    }

    let api = IClassApi::new(config.use_vpn)?;

    let session = match api.login_with_diagnostic(&config.login_input()).await {
        Ok(session) => session,
        Err(diagnostic) => {

            if args.debug_login {

                eprintln!("{}", serde_json::to_string_pretty(&diagnostic)?);
            }

            return Err(anyhow!(diagnostic.summary));
        }
    };

    session
        .bykc_api
        .ok_or_else(|| anyhow!("登录成功但没有建立博雅课程会话，请确认 use_vpn = true"))
}

pub(crate) async fn bykc_courses_command(args: BykcCoursesArgs) -> Result<()> {

    let api = bykc_api(&args.read).await?;

    let courses = api.get_courses(args.all).await?;

    if args.read.json {

        println!("{}", serde_json::to_string_pretty(&courses)?);

        return Ok(());
    }

    if courses.is_empty() {

        println!("没有可报名的博雅课程（加 --all 查看全部）");

        return Ok(());
    }

    println!("course_id\t状态\t已选\t人数\t课程\t开始\t地点\t类别");

    for course in &courses {

        println!(
            "{}\t{}\t{}\t{}/{}\t{}\t{}\t{}\t{}",
            course.id,
            course.status,
            if course.selected { "是" } else { "否" },
            course.course_current_count,
            course.course_max_count,
            course.course_name,
            course.course_start_date,
            course.course_position,
            category_label(&course.category, &course.sub_category),
        );
    }

    Ok(())
}

pub(crate) async fn bykc_chosen_command(args: BykcReadArgs) -> Result<()> {

    let api = bykc_api(&args).await?;

    let courses = api.get_chosen_courses().await?;

    if args.json {

        println!("{}", serde_json::to_string_pretty(&courses)?);

        return Ok(());
    }

    if courses.is_empty() {

        println!("本学期没有已选的博雅课程");

        return Ok(());
    }

    println!("course_id\t课程\t开始\t结束\t退选截止\t可签到\t可签退\t签到信息");

    for course in &courses {

        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            course.course_id,
            course.course_name,
            course.course_start_date,
            course.course_end_date,
            course.course_cancel_end_date,
            course.can_sign,
            course.can_sign_out,
            course.sign_info,
        );
    }

    Ok(())
}

pub(crate) async fn bykc_detail_command(args: BykcCourseArgs) -> Result<()> {

    let api = bykc_api(&args.read).await?;

    let detail = api.get_course_detail(args.course).await?;

    if args.read.json {

        println!("{}", serde_json::to_string_pretty(&detail)?);

        return Ok(());
    }

    print_detail(&detail);

    Ok(())
}

pub(crate) async fn bykc_stats_command(args: BykcReadArgs) -> Result<()> {

    let api = bykc_api(&args).await?;

    let stats = api.get_statistics().await?;

    if args.json {

        println!("{}", serde_json::to_string_pretty(&stats)?);

        return Ok(());
    }

    println!("有效课程总数\t{}", stats.total_valid_count);

    println!("类别\t已通过/要求\t达标");

    for row in &stats.categories {

        println!(
            "{}\t{}/{}\t{}",
            category_label(&row.category_name, &row.sub_category),
            row.passed_count,
            row.required_count,
            if row.is_qualified { "是" } else { "否" },
        );
    }

    Ok(())
}

/// Which way an enrollment write goes.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]

enum Enrollment {
    Select,
    Deselect,
}

impl Enrollment {
    fn command(self) -> &'static str {

        match self {
            Self::Select => "bykc-select",
            Self::Deselect => "bykc-deselect",
        }
    }
}

pub(crate) async fn bykc_select_command(args: BykcWriteArgs) -> Result<()> {

    enrollment_command(args, Enrollment::Select).await
}

pub(crate) async fn bykc_deselect_command(args: BykcWriteArgs) -> Result<()> {

    enrollment_command(args, Enrollment::Deselect).await
}

/// Shared preview-then-write flow for enrollment changes.
///
/// How:
/// The preview re-reads the course so it reports the current state, and it
/// refuses up front when the write cannot succeed (already enrolled, not
/// enrolled), so a planning call already tells the caller the outcome.

async fn enrollment_command(args: BykcWriteArgs, kind: Enrollment) -> Result<()> {

    let api = bykc_api(&args.read).await?;

    let detail = api
        .get_course_detail(args.course)
        .await
        .with_context(|| format!("读取博雅课程 {} 失败", args.course))?;

    check_enrollment(&detail, kind)?;

    if !args.yes {

        if args.read.json {

            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "action": kind.command(),
                    "submitted": false,
                    "course": preview(&detail),
                    "hint": "加上 --yes 才会真正提交",
                }))?
            );

            return Ok(());
        }

        print_detail(&detail);

        println!();

        println!("这是预览。确认无误后加上 --yes 才会真正提交。");

        return Ok(());
    }

    let message = match kind {
        Enrollment::Select => api.select_course(args.course).await,
        Enrollment::Deselect => api.deselect_course(args.course).await,
    }
    .with_context(|| format!("{} {} 失败", kind.command(), args.course))?;

    if args.read.json {

        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "action": kind.command(),
                "submitted": true,
                "course_id": args.course,
                "course_name": detail.course_name,
                "message": message,
            }))?
        );
    } else {

        println!("{message}\t{}\t{}", args.course, detail.course_name);
    }

    Ok(())
}

/// Rejects writes that the current enrollment state makes pointless.

fn check_enrollment(detail: &BykcCourseDetail, kind: Enrollment) -> Result<()> {

    match kind {
        Enrollment::Select if detail.selected => {

            bail!("已报名该课程，无需重复报名: {}", detail.course_name)
        }
        Enrollment::Deselect if !detail.selected => {

            bail!("该课程未报名，无法退选: {}", detail.course_name)
        }
        _ => Ok(()),
    }
}

fn preview(detail: &BykcCourseDetail) -> serde_json::Value {

    json!({
        "course_id": detail.id,
        "course_name": detail.course_name,
        "status": detail.status,
        "selected": detail.selected,
        "count": detail.course_current_count,
        "max": detail.course_max_count,
        "start": detail.course_start_date,
        "select_end": detail.course_select_end_date,
        "cancel_end": detail.course_cancel_end_date,
    })
}

fn print_detail(detail: &BykcCourseDetail) {

    println!("course_id\t{}", detail.id);

    println!("课程\t{}", detail.course_name);

    println!("状态\t{}", detail.status);

    println!("已报名\t{}", if detail.selected { "是" } else { "否" });

    println!(
        "人数\t{}/{}",
        detail.course_current_count, detail.course_max_count
    );

    println!(
        "时间\t{} - {}",
        detail.course_start_date, detail.course_end_date
    );

    println!(
        "报名\t{} - {}",
        detail.course_select_start_date, detail.course_select_end_date
    );

    println!("退选截止\t{}", detail.course_cancel_end_date);

    println!("地点\t{}", detail.course_position);

    println!("教师\t{}", detail.course_teacher);

    println!(
        "类别\t{}",
        category_label(&detail.category, &detail.sub_category)
    );

    if let Some(config) = &detail.sign_config {

        println!(
            "签到窗口\t{} - {}",
            config.sign_start_date, config.sign_end_date
        );

        println!(
            "签退窗口\t{} - {}",
            config.sign_out_start_date, config.sign_out_end_date
        );
    }
}

fn category_label(category: &str, sub_category: &str) -> String {

    if sub_category.is_empty() || sub_category == category {

        category.to_string()
    } else {

        format!("{category}/{sub_category}")
    }
}

#[cfg(test)]

mod tests {

    use super::{Enrollment, category_label, check_enrollment};
    use crate::bykc::BykcCourseDetail;

    fn detail(selected: bool) -> BykcCourseDetail {

        BykcCourseDetail {
            id: 7,
            course_name: "测试课程".to_string(),
            selected,
            ..Default::default()
        }
    }

    #[test]

    fn selecting_an_enrolled_course_is_refused_before_any_write() {

        assert!(check_enrollment(&detail(true), Enrollment::Select).is_err());

        assert!(check_enrollment(&detail(false), Enrollment::Select).is_ok());
    }

    #[test]

    fn withdrawing_requires_an_existing_enrollment() {

        assert!(check_enrollment(&detail(false), Enrollment::Deselect).is_err());

        assert!(check_enrollment(&detail(true), Enrollment::Deselect).is_ok());
    }

    #[test]

    fn category_label_omits_a_redundant_sub_category() {

        assert_eq!(category_label("美育", ""), "美育");

        assert_eq!(category_label("美育", "美育"), "美育");

        assert_eq!(category_label("德育", "安全"), "德育/安全");
    }
}
