//! Academic schedule integration and its account-scoped offline cache.
//!
//! The upstream UBAA schedule slice has two important properties that are
//! useful for a terminal client as well: undergraduate and graduate accounts
//! use different upstreams, and reading a saved timetable must never trigger a
//! network request.  This module keeps both concerns behind one small model so
//! the TUI does not need to know which portal supplied a course.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{Datelike, Duration, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;

use crate::constants::{
    BYXT_CURRENT_USER_URL, BYXT_HOME_URL, BYXT_TERMS_URL, BYXT_WEEK_URL, BYXT_WEEKS_URL,
    GSMIS_HOME_URL, GSMIS_SCHEDULE_URL, GSMIS_TERMS_URL, to_webvpn_url,
};
use crate::iclass::IClassApi;
use crate::model::Session;

const SCHEDULE_CACHE_VERSION: u8 = 1;

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]

pub struct Term {
    pub code:     String,
    pub name:     String,
    pub selected: bool,
    pub index:    usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]

pub struct Week {
    pub start_date: String,
    pub end_date:   String,
    pub term_code:  String,
    pub current:    bool,
    pub number:     usize,
    pub name:       String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]

pub struct SectionTime {
    pub section: usize,
    pub start:   Option<String>,
    pub end:     Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]

pub struct ScheduleEntry {
    pub course_code:        String,
    pub course_name:        String,
    pub course_serial_no:   Option<String>,
    pub credit:             Option<String>,
    pub begin_time:         Option<String>,
    pub end_time:           Option<String>,
    pub begin_section:      Option<usize>,
    pub end_section:        Option<usize>,
    pub place:              Option<String>,
    pub weeks_and_teachers: Option<String>,
    pub teaching_target:    Option<String>,
    pub color:              Option<String>,
    pub day_of_week:        Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]

pub struct WeeklySchedule {
    pub entries:       Vec<ScheduleEntry>,
    pub term_code:     String,
    pub term_name:     String,
    pub section_times: Vec<SectionTime>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]

pub struct SemesterSchedule {
    pub terms:      Vec<Term>,
    pub term_code:  String,
    pub weeks:      Vec<Week>,
    pub schedules:  BTreeMap<usize, WeeklySchedule>,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]

struct ScheduleCache {
    version:   u8,
    semesters: Vec<SemesterSchedule>,
}

#[derive(Clone, Debug)]

pub(crate) struct ScheduleHttpResponse {
    pub status:    u16,
    pub final_url: String,
    pub body:      String,
}

impl SemesterSchedule {
    pub fn schedule_for(&self, number: usize) -> Option<&WeeklySchedule> {

        self.schedules.get(&number)
    }
}

impl IClassApi {
    /// Imports every week of one academic term, preferring the undergraduate
    /// portal and falling back to GSMIS for graduate accounts.

    pub async fn import_semester_schedule(
        &self,
        _session: &Session,
        requested_term: Option<&str>,
    ) -> Result<SemesterSchedule> {

        if self.undergraduate_portal_ready().await.unwrap_or(false) {

            return self
                .import_undergraduate_schedule(requested_term)
                .await
                .context("本科课表导入失败");
        }

        self.import_graduate_schedule(requested_term)
            .await
            .context("研究生课表导入失败")
    }

    async fn undergraduate_portal_ready(&self) -> Result<bool> {

        let response = self
            .schedule_get(&schedule_url(self.use_vpn, BYXT_CURRENT_USER_URL))
            .await?;

        if response.status != 200 || looks_like_sso_response(&response) {

            return Ok(false);
        }

        let body = response.body.trim_start();

        if !body.starts_with(['{', '[']) {

            return Ok(false);
        }

        let value: Value = match serde_json::from_str(body) {
            Ok(value) => value,
            Err(_) => return Ok(false),
        };

        Ok(value
            .get("code")
            .and_then(scalar_string)
            .is_none_or(|code| code == "0"))
    }

    async fn import_undergraduate_schedule(
        &self,
        requested_term: Option<&str>,
    ) -> Result<SemesterSchedule> {

        let terms = parse_undergraduate_terms(
            &self
                .schedule_get(&schedule_url(self.use_vpn, BYXT_TERMS_URL))
                .await?,
        )?;

        let term = choose_term(&terms, requested_term)?;

        let term_code = term.code.clone();

        let term_name = term.name.clone();

        let weeks = parse_undergraduate_weeks(
            &self
                .schedule_get(&with_query(
                    &schedule_url(self.use_vpn, BYXT_WEEKS_URL),
                    "termCode",
                    &term_code,
                ))
                .await?,
            &term_code,
        )?;

        if weeks.is_empty() {

            bail!("本科系统未返回教学周");
        }

        let mut tasks = Vec::with_capacity(weeks.len());

        for week in &weeks {

            let api = self.clone();

            let term_code = term_code.clone();

            let term_name = term_name.clone();

            let number = week.number;

            tasks.push(tokio::spawn(async move {

                let response = api
                    .schedule_post_form(
                        &schedule_url(api.use_vpn, BYXT_WEEK_URL),
                        &[
                            ("termCode", term_code.as_str()),
                            ("type", "week"),
                            ("week", &number.to_string()),
                        ],
                    )
                    .await?;

                parse_weekly_schedule(&response, &term_code, &term_name)
                    .with_context(|| format!("解析本科第 {number} 周课表失败"))
            }));
        }

        let mut schedules = BTreeMap::new();

        for (week, task) in weeks.iter().zip(tasks) {

            let schedule = task.await.context("本科课表周次任务失败")??;

            schedules.insert(week.number, schedule);
        }

        Ok(SemesterSchedule {
            terms,
            term_code,
            weeks,
            schedules,
            updated_at: now_text(),
        })
    }

    async fn import_graduate_schedule(
        &self,
        requested_term: Option<&str>,
    ) -> Result<SemesterSchedule> {

        let home = self
            .schedule_get(&schedule_url(self.use_vpn, GSMIS_HOME_URL))
            .await?;

        ensure_schedule_response(&home, "研究生登录")?;

        let terms = parse_graduate_terms(
            &self
                .schedule_post_form(&schedule_url(self.use_vpn, GSMIS_TERMS_URL), &[])
                .await?,
        )?;

        let term = choose_term(&terms, requested_term)?;

        let term_code = term.code.clone();

        let response = self
            .schedule_post_form(
                &schedule_url(self.use_vpn, GSMIS_SCHEDULE_URL),
                &[
                    ("ZC", ""),
                    ("XNXQDM", term_code.as_str()),
                    ("XH", ""),
                    ("XQDM", ""),
                ],
            )
            .await?;

        let (weeks, schedules) = parse_graduate_schedule(&response, term)?;

        if weeks.is_empty() {

            bail!("GSMIS 未返回可用教学周");
        }

        Ok(SemesterSchedule {
            terms,
            term_code,
            weeks,
            schedules,
            updated_at: now_text(),
        })
    }

    pub(crate) async fn schedule_get(&self, url: &str) -> Result<ScheduleHttpResponse> {

        let response = self
            .client
            .get(url)
            .header("Accept", "application/json, text/javascript, */*; q=0.01")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", schedule_url(self.use_vpn, BYXT_HOME_URL))
            .send()
            .await
            .with_context(|| format!("请求课表接口失败: {url}"))?;

        let status = response.status().as_u16();

        let final_url = response.url().to_string();

        let body = response.text().await.context("读取课表接口响应失败")?;

        Ok(ScheduleHttpResponse {
            status,
            final_url,
            body,
        })
    }

    pub(crate) async fn schedule_post_form(
        &self,
        url: &str,
        form: &[(&str, &str)],
    ) -> Result<ScheduleHttpResponse> {

        let response = self
            .client
            .post(url)
            .header("Accept", "application/json, text/javascript, */*; q=0.01")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", schedule_url(self.use_vpn, BYXT_HOME_URL))
            .form(form)
            .send()
            .await
            .with_context(|| format!("请求课表接口失败: {url}"))?;

        let status = response.status().as_u16();

        let final_url = response.url().to_string();

        let body = response.text().await.context("读取课表接口响应失败")?;

        Ok(ScheduleHttpResponse {
            status,
            final_url,
            body,
        })
    }
}

fn schedule_url(use_vpn: bool, raw: &str) -> String {

    if use_vpn {

        to_webvpn_url(raw)
    } else {

        raw.to_string()
    }
}

fn with_query(base: &str, key: &str, value: &str) -> String {

    format!("{base}?{}={}", urlencoding(key), urlencoding(value))
}

fn urlencoding(value: &str) -> String {

    value
        .bytes()
        .map(|byte| {

            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (byte as char).to_string()
                }
                _ => format!("%{byte:02X}"),
            }
        })
        .collect()
}

fn ensure_schedule_response(response: &ScheduleHttpResponse, stage: &str) -> Result<()> {

    if looks_like_sso_response(response) {

        bail!("{stage}失败：统一认证会话已失效，请重新登录");
    }

    if !(200..300).contains(&response.status) {

        bail!("{stage}失败：HTTP {}", response.status);
    }

    Ok(())
}

fn looks_like_sso_response(response: &ScheduleHttpResponse) -> bool {

    let lower_url = response.final_url.to_ascii_lowercase();

    let body = response.body.to_ascii_lowercase();

    lower_url.contains("sso.buaa.edu.cn/login")
        || body.contains("name=\"execution\"")
        || body.contains("name='execution'")
        || body.contains("统一身份认证")
}

fn choose_term<'a>(terms: &'a [Term], requested: Option<&str>) -> Result<&'a Term> {

    requested
        .and_then(|code| terms.iter().find(|term| term.code == code))
        .or_else(|| terms.iter().find(|term| term.selected))
        .or_else(|| terms.first())
        .ok_or_else(|| anyhow!("系统未返回可用学期"))
}

fn parse_undergraduate_terms(response: &ScheduleHttpResponse) -> Result<Vec<Term>> {

    ensure_schedule_response(response, "本科课表学期列表")?;

    let root = parse_json_body(&response.body, "本科课表学期列表")?;

    let rows = array_at(&root, &["datas"])
        .or_else(|| array_at(&root, &["data"]))
        .ok_or_else(|| anyhow!("本科课表学期列表格式异常"))?;

    let mut terms = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {

            let code = first_string(row, &["itemCode", "termCode", "code", "XNXQDM"])?;

            let name = first_string(row, &["itemName", "termName", "name", "XNXQMC"])
                .unwrap_or_else(|| code.clone());

            let selected = first_bool(row, &["selected", "current", "curTerm"]).unwrap_or(false);

            Some(Term {
                code,
                name,
                selected,
                index,
            })
        })
        .collect::<Vec<_>>();

    if terms.is_empty() {

        bail!("本科课表学期列表为空");
    }

    if !terms.iter().any(|term| term.selected) {

        terms[0].selected = true;
    }

    Ok(terms)
}

fn parse_undergraduate_weeks(
    response: &ScheduleHttpResponse,
    term_code: &str,
) -> Result<Vec<Week>> {

    ensure_schedule_response(response, "本科课表周次列表")?;

    let root = parse_json_body(&response.body, "本科课表周次列表")?;

    let rows = array_at(&root, &["datas"])
        .or_else(|| array_at(&root, &["data"]))
        .ok_or_else(|| anyhow!("本科课表周次列表格式异常"))?;

    let today = Local::now().date_naive();

    let weeks = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {

            let number = first_usize(row, &["serialNumber", "week", "weekNo", "number"])
                .unwrap_or(index + 1);

            let start_date = first_string(row, &["startDate", "start", "beginDate"])?;

            let end_date = first_string(row, &["endDate", "end", "finishDate"])?;

            Some(Week {
                current: date_in_range(today, &start_date, &end_date),
                start_date,
                end_date,
                term_code: term_code.to_string(),
                number,
                name: first_string(row, &["name", "weekName"])
                    .unwrap_or_else(|| format!("第 {number} 周")),
            })
        })
        .collect::<Vec<_>>();

    Ok(weeks)
}

fn parse_weekly_schedule(
    response: &ScheduleHttpResponse,
    term_code: &str,
    term_name: &str,
) -> Result<WeeklySchedule> {

    ensure_schedule_response(response, "本科周课表")?;

    let root = parse_json_body(&response.body, "本科周课表")?;

    let data = root
        .get("datas")
        .or_else(|| root.get("data"))
        .unwrap_or(&root);

    let rows = data
        .get("arrangedList")
        .or_else(|| data.get("arranged"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(WeeklySchedule {
        entries:       rows
            .iter()
            .map(parse_schedule_entry)
            .collect::<Result<_>>()?,
        term_code:     term_code.to_string(),
        term_name:     term_name.to_string(),
        section_times: parse_section_times(data),
    })
}

fn parse_graduate_terms(response: &ScheduleHttpResponse) -> Result<Vec<Term>> {

    ensure_schedule_response(response, "GSMIS 学期列表")?;

    let root = parse_json_body(&response.body, "GSMIS 学期列表")?;

    let rows = root
        .pointer("/datas/kfdxnxqcx/rows")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("GSMIS 学期列表格式异常"))?;

    let mut terms = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {

            let code = required_string(row, &["XNXQDM"], "研究生学期代码")?;

            let name =
                first_string(row, &["XNXQDM_DISPLAY", "XNXQMC"]).unwrap_or_else(|| code.clone());

            Ok(Term {
                code,
                name,
                selected: index == 0,
                index,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    terms.sort_by(|left, right| right.code.cmp(&left.code));

    for (index, term) in terms.iter_mut().enumerate() {

        term.index = index;

        term.selected = index == 0;
    }

    Ok(terms)
}

fn parse_graduate_schedule(
    response: &ScheduleHttpResponse,
    term: &Term,
) -> Result<(Vec<Week>, BTreeMap<usize, WeeklySchedule>)> {

    ensure_schedule_response(response, "GSMIS 课表")?;

    let root = parse_json_body(&response.body, "GSMIS 课表")?;

    if root.get("code").and_then(scalar_string).as_deref() != Some("1") {

        bail!("GSMIS 课表返回失败");
    }

    let courses = root
        .get("rwList")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("GSMIS 课表缺少 rwList"))?;

    let rows = root
        .get("jgList")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("GSMIS 课表缺少 jgList"))?;

    let slots = root
        .get("jcfaList")
        .and_then(Value::as_array)
        .map(|schemes| {

            schemes
                .iter()
                .filter_map(|scheme| scheme.get("skjcList"))
                .filter_map(Value::as_array)
                .flat_map(|items| items.iter())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let course_by_class = courses
        .iter()
        .filter_map(|course| first_string(course, &["BJDM"]).map(|id| (id, course)))
        .collect::<HashMap<_, _>>();

    let mut normalized = Vec::new();

    let mut max_week = 0usize;

    let mut start_candidates = Vec::new();

    for row in rows {

        let class_id = required_string(row, &["BJDM"], "GSMIS 教学班号")?;

        let Some(course) = course_by_class.get(&class_id) else {

            bail!("GSMIS 排课缺少课程信息");
        };

        let mask = required_string(row, &["ZCBH"], "GSMIS 周次位图")?;

        if mask.is_empty() || !mask.chars().all(|char| matches!(char, '0' | '1')) {

            bail!("GSMIS 周次位图无效");
        }

        max_week = max_week.max(mask.len());

        let day = required_usize(row, &["XQ"], "GSMIS 星期")?;

        let begin_section = required_usize(row, &["KSJCDM"], "GSMIS 起始节次")?;

        let end_section = required_usize(row, &["JSJCDM"], "GSMIS 结束节次")?;

        if !(1..=7).contains(&day) || begin_section == 0 || end_section < begin_section {

            bail!("GSMIS 排课节次或星期无效");
        }

        if let Some(first_week) = mask.find('1') {

            if let Some(first_date) = first_string(course, &["SCSKRQ"]) {

                let date = NaiveDate::parse_from_str(&first_date, "%Y-%m-%d")
                    .with_context(|| format!("GSMIS 首次上课日期无效: {first_date}"))?;

                start_candidates.push(date - Duration::days((first_week * 7 + day - 1) as i64));
            }
        }

        let scheme = first_string(row, &["JCFADM"]);

        let begin_time = find_slot_time(&slots, scheme.as_deref(), begin_section, true)?;

        let end_time = find_slot_time(&slots, scheme.as_deref(), end_section, false)?;

        normalized.push((
            mask,
            ScheduleEntry {
                course_code: first_string(row, &["KCDM"]).unwrap_or_default(),
                course_name: first_string(row, &["KCMC"])
                    .or_else(|| first_string(course, &["KCMC"]))
                    .unwrap_or_else(|| "未命名课程".to_string()),
                course_serial_no: Some(class_id),
                credit: first_string(course, &["XF"]),
                begin_time,
                end_time,
                begin_section: Some(begin_section),
                end_section: Some(end_section),
                place: first_string(row, &["JASMC"]),
                weeks_and_teachers: Some(
                    [
                        first_string(row, &["ZCMC"]),
                        first_string(row, &["JGJSXM", "JSXM"]),
                    ]
                    .into_iter()
                    .flatten()
                    .filter(|value| !value.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join(" "),
                ),
                teaching_target: None,
                color: None,
                day_of_week: Some(day),
            },
        ));
    }

    let start_date = start_candidates
        .first()
        .copied()
        .or_else(|| Some(monday_of(Local::now().date_naive())))
        .ok_or_else(|| anyhow!("GSMIS 无法推导学期开始日期"))?;

    if start_candidates
        .iter()
        .any(|candidate| *candidate != start_date)
    {

        bail!("GSMIS 课表日期与周次无法一致对应");
    }

    let section_times = parse_graduate_section_times(&slots)?;

    let mut schedules = BTreeMap::new();

    for number in 1..=max_week {

        let entries = normalized
            .iter()
            .filter(|(mask, _)| mask.as_bytes().get(number - 1) == Some(&b'1'))
            .map(|(_, entry)| entry.clone())
            .collect::<Vec<_>>();

        schedules.insert(
            number,
            WeeklySchedule {
                entries,
                term_code: term.code.clone(),
                term_name: term.name.clone(),
                section_times: section_times.clone(),
            },
        );
    }

    let today = Local::now().date_naive();

    let weeks = (1..=max_week)
        .map(|number| {

            let start = start_date + Duration::days(((number - 1) * 7) as i64);

            let end = start + Duration::days(6);

            Week {
                start_date: start.to_string(),
                end_date: end.to_string(),
                term_code: term.code.clone(),
                current: today >= start && today <= end,
                number,
                name: format!("第 {number} 周"),
            }
        })
        .collect();

    Ok((weeks, schedules))
}

fn parse_schedule_entry(row: &Value) -> Result<ScheduleEntry> {

    Ok(ScheduleEntry {
        course_code:        first_string(row, &["courseCode", "courseNo", "KCDM"])
            .unwrap_or_default(),
        course_name:        first_string(row, &["courseName", "KCMC", "name"])
            .unwrap_or_else(|| "未命名课程".to_string()),
        course_serial_no:   first_string(row, &["courseSerialNo", "classNo", "BJDM"]),
        credit:             first_string(row, &["credit", "XF"]),
        begin_time:         first_string(row, &["beginTime", "startTime", "KSSJ"]),
        end_time:           first_string(row, &["endTime", "finishTime", "JSSJ"]),
        begin_section:      first_usize(row, &["beginSection", "startSection", "KSJCDM"]),
        end_section:        first_usize(row, &["endSection", "finishSection", "JSJCDM"]),
        place:              first_string(row, &["placeName", "place", "JASMC"]),
        weeks_and_teachers: first_string(row, &["weeksAndTeachers", "teacher", "JSXM"]),
        teaching_target:    first_string(row, &["teachingTarget"]),
        color:              first_string(row, &["color"]),
        day_of_week:        first_usize(row, &["dayOfWeek", "weekday", "XQ"]),
    })
}

fn parse_section_times(value: &Value) -> Vec<SectionTime> {

    value
        .get("sectionTimes")
        .or_else(|| value.get("sectionTime"))
        .and_then(Value::as_array)
        .map(|rows| {

            rows.iter()
                .filter_map(|row| {

                    Some(SectionTime {
                        section: first_usize(row, &["section", "dm"])?,
                        start:   first_string(row, &["start", "startTime", "KSSJ"]),
                        end:     first_string(row, &["end", "endTime", "JSSJ"]),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_graduate_section_times(slots: &[&Value]) -> Result<Vec<SectionTime>> {

    let mut result = Vec::new();

    for slot in slots {

        let section = required_usize(slot, &["DM"], "GSMIS 节次")?;

        let start = required_time(slot, &["KSSJ"], "GSMIS 上课时间")?;

        let end = required_time(slot, &["JSSJ"], "GSMIS 下课时间")?;

        if !result
            .iter()
            .any(|item: &SectionTime| item.section == section)
        {

            result.push(SectionTime {
                section,
                start: Some(start),
                end: Some(end),
            });
        }
    }

    result.sort_by_key(|item| item.section);

    Ok(result)
}

fn find_slot_time(
    slots: &[&Value],
    scheme: Option<&str>,
    section: usize,
    start: bool,
) -> Result<Option<String>> {

    let slot = slots.iter().find(|slot| {

        first_usize(slot, &["DM"]) == Some(section)
            && (scheme.is_none() || first_string(slot, &["JCFADM"]).as_deref() == scheme)
    });

    slot.map(|slot| {

        required_time(
            slot,
            &[if start { "KSSJ" } else { "JSSJ" }],
            "GSMIS 节次时间",
        )
    })
    .transpose()
}

fn required_time(row: &Value, keys: &[&str], label: &str) -> Result<String> {

    let number = required_usize(row, keys, label)?;

    if number > 2359 || number % 100 > 59 {

        bail!("{label}无效");
    }

    Ok(format!("{:02}:{:02}", number / 100, number % 100))
}

fn parse_json_body(body: &str, stage: &str) -> Result<Value> {

    serde_json::from_str(body)
        .with_context(|| format!("{stage}响应不是合法 JSON（仅保留响应长度 {}）", body.len()))
}

fn array_at<'a>(root: &'a Value, keys: &[&str]) -> Option<&'a Vec<Value>> {

    let mut value = root;

    for key in keys {

        value = value.get(*key)?;
    }

    value.as_array()
}

fn first_string(row: &Value, keys: &[&str]) -> Option<String> {

    keys.iter()
        .find_map(|key| row.get(*key).and_then(scalar_string))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn required_string(row: &Value, keys: &[&str], label: &str) -> Result<String> {

    first_string(row, keys).ok_or_else(|| anyhow!("{label}缺少字段"))
}

fn scalar_string(value: &Value) -> Option<String> {

    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn first_usize(row: &Value, keys: &[&str]) -> Option<usize> {

    keys.iter().find_map(|key| {

        row.get(*key).and_then(|value| {

            match value {
                Value::Number(number) => number.as_u64().map(|value| value as usize),
                Value::String(value) => value.trim().parse().ok(),
                _ => None,
            }
        })
    })
}

fn required_usize(row: &Value, keys: &[&str], label: &str) -> Result<usize> {

    first_usize(row, keys).ok_or_else(|| anyhow!("{label}缺少字段"))
}

fn first_bool(row: &Value, keys: &[&str]) -> Option<bool> {

    keys.iter().find_map(|key| {

        row.get(*key).and_then(|value| {

            match value {
                Value::Bool(value) => Some(*value),
                Value::Number(value) => value.as_i64().map(|value| value != 0),
                Value::String(value) => {
                    match value.trim() {
                        "1" | "true" | "TRUE" => Some(true),
                        "0" | "false" | "FALSE" => Some(false),
                        _ => None,
                    }
                }
                _ => None,
            }
        })
    })
}

fn date_in_range(today: NaiveDate, start: &str, end: &str) -> bool {

    NaiveDate::parse_from_str(start, "%Y-%m-%d")
        .ok()
        .zip(NaiveDate::parse_from_str(end, "%Y-%m-%d").ok())
        .is_some_and(|(start, end)| today >= start && today <= end)
}

fn monday_of(date: NaiveDate) -> NaiveDate {

    date - Duration::days(date.weekday().num_days_from_monday() as i64)
}

fn now_text() -> String {

    Local::now().format("%Y-%m-%d %H:%M").to_string()
}

pub fn load_cached_schedules(account: &str) -> Result<Vec<SemesterSchedule>> {

    let path = schedule_cache_path(account)?;

    if !path.is_file() {

        return Ok(Vec::new());
    }

    let raw = fs::read_to_string(&path)
        .with_context(|| format!("读取离线课表失败: {}", path.display()))?;

    let cache: ScheduleCache = serde_json::from_str(&raw)
        .with_context(|| format!("解析离线课表失败: {}", path.display()))?;

    if cache.version != SCHEDULE_CACHE_VERSION {

        bail!("离线课表版本不兼容，请重新更新课表");
    }

    Ok(cache.semesters)
}

pub fn save_cached_schedule(account: &str, schedule: SemesterSchedule) -> Result<()> {

    let path = schedule_cache_path(account)?;

    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("无法确定离线课表目录"))?;

    fs::create_dir_all(parent)
        .with_context(|| format!("创建离线课表目录失败: {}", parent.display()))?;

    let mut semesters = load_cached_schedules(account)?;

    semesters.retain(|item| item.term_code != schedule.term_code);

    semesters.push(schedule);

    semesters.sort_by(|left, right| right.term_code.cmp(&left.term_code));

    let body = serde_json::to_string_pretty(&ScheduleCache {
        version: SCHEDULE_CACHE_VERSION,
        semesters,
    })?;

    let temporary = parent.join(format!(".schedule-{}.tmp", std::process::id()));

    fs::write(&temporary, body)
        .with_context(|| format!("写入离线课表临时文件失败: {}", temporary.display()))?;

    if let Err(error) = fs::rename(&temporary, &path) {

        let _ = fs::remove_file(&temporary);

        return Err(error).with_context(|| format!("发布离线课表失败: {}", path.display()));
    }

    Ok(())
}

pub fn schedule_cache_path(account: &str) -> Result<PathBuf> {

    let base = if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {

        PathBuf::from(path)
    } else if let Some(path) = std::env::var_os("HOME") {

        PathBuf::from(path).join(".config")
    } else {

        bail!("找不到配置目录");
    };

    let digest = Sha1::digest(account.as_bytes());

    let name = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    Ok(base
        .join("iclass-buaa")
        .join("schedules")
        .join(format!("{name}.json")))
}

pub fn cached_semester<'a>(
    semesters: &'a [SemesterSchedule],
    term_code: Option<&str>,
) -> Result<&'a SemesterSchedule> {

    term_code
        .and_then(|code| semesters.iter().find(|item| item.term_code == code))
        .or_else(|| {

            semesters
                .iter()
                .find(|item| item.weeks.iter().any(|week| week.current))
        })
        .or_else(|| semesters.first())
        .ok_or_else(|| anyhow!("没有已缓存课表，请先在 TUI 中按 u 导入整学期课表"))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]

pub struct ScheduleChange {
    pub kind:        String,
    pub week:        usize,
    pub day:         Option<usize>,
    pub course_name: String,
    pub detail:      String,
}

pub fn diff_semesters(from: &SemesterSchedule, to: &SemesterSchedule) -> Vec<ScheduleChange> {

    let mut changes = Vec::new();

    for week in from
        .schedules
        .keys()
        .chain(to.schedules.keys())
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
    {

        let before = from
            .schedules
            .get(&week)
            .map(|item| &item.entries)
            .cloned()
            .unwrap_or_default();

        let after = to
            .schedules
            .get(&week)
            .map(|item| &item.entries)
            .cloned()
            .unwrap_or_default();

        for entry in &after {

            if !before.contains(entry) {

                changes.push(ScheduleChange {
                    kind: "added".to_string(),
                    week,
                    day: entry.day_of_week,
                    course_name: entry.course_name.clone(),
                    detail: entry_detail(entry),
                });
            }
        }

        for entry in &before {

            if !after.contains(entry) {

                changes.push(ScheduleChange {
                    kind: "removed".to_string(),
                    week,
                    day: entry.day_of_week,
                    course_name: entry.course_name.clone(),
                    detail: entry_detail(entry),
                });
            }
        }
    }

    changes.sort_by(|left, right| {

        (left.week, left.day, &left.course_name, &left.kind).cmp(&(
            right.week,
            right.day,
            &right.course_name,
            &right.kind,
        ))
    });

    changes
}

pub fn render_schedule_export(schedule: &SemesterSchedule, format: &str) -> Result<String> {

    match format {
        "markdown" => Ok(render_markdown(schedule)),
        "csv" => Ok(render_csv(schedule)),
        "json" => Ok(serde_json::to_string_pretty(schedule)?),
        "ics" => Ok(render_ics(schedule)),
        other => bail!("不支持的课表导出格式: {other}"),
    }
}

fn entry_detail(entry: &ScheduleEntry) -> String {

    format!(
        "{} {}-{} {}",
        entry.begin_time.as_deref().unwrap_or("--:--"),
        entry
            .begin_section
            .map(|value| value.to_string())
            .as_deref()
            .unwrap_or("-"),
        entry
            .end_section
            .map(|value| value.to_string())
            .as_deref()
            .unwrap_or("-"),
        entry.place.as_deref().unwrap_or("未填写地点"),
    )
}

fn render_markdown(schedule: &SemesterSchedule) -> String {

    let mut output = format!("# {}\n\n", schedule.term_code);

    for week in &schedule.weeks {

        output.push_str(&format!(
            "## {} {} ~ {}\n\n",
            week.name, week.start_date, week.end_date
        ));

        output.push_str("| 星期 | 时间 | 课程 | 地点 | 教师 |\n| --- | --- | --- | --- | --- |\n");

        if let Some(weekly) = schedule.schedules.get(&week.number) {

            for entry in &weekly.entries {

                output.push_str(&format!(
                    "| {} | {}-{} | {} | {} | {} |\n",
                    entry
                        .day_of_week
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    entry.begin_time.as_deref().unwrap_or("--:--"),
                    entry.end_time.as_deref().unwrap_or("--:--"),
                    entry.course_name,
                    entry.place.as_deref().unwrap_or(""),
                    entry.weeks_and_teachers.as_deref().unwrap_or(""),
                ));
            }
        }

        output.push('\n');
    }

    output
}

fn render_csv(schedule: &SemesterSchedule) -> String {

    let mut output =
        String::from("term,week,day,course_code,course_name,start,end,place,teacher\n");

    for week in &schedule.weeks {

        if let Some(weekly) = schedule.schedules.get(&week.number) {

            for entry in &weekly.entries {

                output.push_str(
                    &[
                        csv_field(&schedule.term_code),
                        week.number.to_string(),
                        entry
                            .day_of_week
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                        csv_field(&entry.course_code),
                        csv_field(&entry.course_name),
                        csv_field(entry.begin_time.as_deref().unwrap_or("")),
                        csv_field(entry.end_time.as_deref().unwrap_or("")),
                        csv_field(entry.place.as_deref().unwrap_or("")),
                        csv_field(entry.weeks_and_teachers.as_deref().unwrap_or("")),
                    ]
                    .join(","),
                );

                output.push('\n');
            }
        }
    }

    output
}

fn render_ics(schedule: &SemesterSchedule) -> String {

    let mut output =
        String::from("BEGIN:VCALENDAR\nVERSION:2.0\nPRODID:-//iclass-buaa-tui//schedule//EN\n");

    for week in &schedule.weeks {

        let Some(start) = NaiveDate::parse_from_str(&week.start_date, "%Y-%m-%d").ok() else {

            continue;
        };

        let Some(weekly) = schedule.schedules.get(&week.number) else {

            continue;
        };

        for entry in &weekly.entries {

            let Some(day) = entry.day_of_week else {

                continue;
            };

            let Some(begin) = parse_hhmm(entry.begin_time.as_deref()) else {

                continue;
            };

            let Some(end) = parse_hhmm(entry.end_time.as_deref()) else {

                continue;
            };

            let date = start + Duration::days(day.saturating_sub(1) as i64);

            output.push_str("BEGIN:VEVENT\n");

            output.push_str(&format!(
                "UID:{}-{}-{}@iclass-buaa-tui\n",
                schedule.term_code, week.number, entry.course_code
            ));

            output.push_str(&format!(
                "DTSTART:{}T{:02}{:02}00\n",
                date.format("%Y%m%d"),
                begin.0,
                begin.1
            ));

            output.push_str(&format!(
                "DTEND:{}T{:02}{:02}00\n",
                date.format("%Y%m%d"),
                end.0,
                end.1
            ));

            output.push_str(&format!("SUMMARY:{}\n", ics_escape(&entry.course_name)));

            if let Some(place) = entry.place.as_deref() {

                output.push_str(&format!("LOCATION:{}\n", ics_escape(place)));
            }

            output.push_str("END:VEVENT\n");
        }
    }

    output.push_str("END:VCALENDAR\n");

    output
}

fn csv_field(value: &str) -> String {

    if value.contains([',', '"', '\n']) {

        format!("\"{}\"", value.replace('"', "\"\""))
    } else {

        value.to_string()
    }
}

fn parse_hhmm(value: Option<&str>) -> Option<(u32, u32)> {

    let (hour, minute) = value?.split_once(':')?;

    Some((hour.parse().ok()?, minute.parse().ok()?))
}

fn ics_escape(value: &str) -> String {

    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
}

#[cfg(test)]

mod tests {

    use super::{
        ScheduleHttpResponse, parse_graduate_schedule, parse_graduate_terms, parse_weekly_schedule,
    };

    #[test]

    fn parses_undergraduate_weekly_schedule() {

        let response = ScheduleHttpResponse {
            status: 200,
            final_url: "https://byxt.buaa.edu.cn/schedule".to_string(),
            body: r#"{"code":"0","datas":{"arrangedList":[{"courseCode":"M001","courseName":"高等数学","beginTime":"08:00","endTime":"09:35","beginSection":1,"endSection":2,"placeName":"主M101","dayOfWeek":1}]}}"#.to_string(),
        };

        let schedule = parse_weekly_schedule(&response, "2026-2027-1", "秋季学期")
            .expect("本科周课表应可解析");

        assert_eq!(schedule.entries.len(), 1);

        assert_eq!(schedule.entries[0].course_name, "高等数学");

        assert_eq!(schedule.entries[0].day_of_week, Some(1));
    }

    #[test]

    fn parses_graduate_schedule_and_keeps_empty_weeks() {

        let terms = ScheduleHttpResponse {
            status: 200,
            final_url: "https://gsmis.buaa.edu.cn/terms".to_string(),
            body: r#"{"code":0,"datas":{"kfdxnxqcx":{"rows":[{"XNXQDM":"20261","XNXQDM_DISPLAY":"2026-2027 学年第一学期"}]}}}"#.to_string(),
        };

        let schedule = ScheduleHttpResponse {
            status: 200,
            final_url: "https://gsmis.buaa.edu.cn/schedule".to_string(),
            body: r#"{"code":1,"rwList":[{"BJDM":"B1","XNXQDM":"20261","KCMC":"研究生课程","SCSKRQ":"2026-09-07","XF":"2"}],"jgList":[{"BJDM":"B1","KCDM":"G001","XQ":1,"KSJCDM":1,"JSJCDM":2,"ZCBH":"0100","JCFADM":"01","JASMC":"学院101","JGJSXM":"张老师"}],"jcfaList":[{"skjcList":[{"JCFADM":"01","DM":1,"KSSJ":800,"JSSJ":845},{"JCFADM":"01","DM":2,"KSSJ":850,"JSSJ":935}]}]}"#.to_string(),
        };

        let term = parse_graduate_terms(&terms)
            .expect("研究生学期应可解析")
            .remove(0);

        let (weeks, schedules) =
            parse_graduate_schedule(&schedule, &term).expect("研究生课表应可解析");

        assert_eq!(weeks.len(), 4);

        assert!(schedules[&1].entries.is_empty());

        assert_eq!(
            schedules[&2].entries[0].begin_time.as_deref(),
            Some("08:00")
        );

        assert_eq!(schedules[&2].entries[0].end_time.as_deref(), Some("09:35"));
    }
}
