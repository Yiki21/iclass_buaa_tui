---
name: buaa-academics
description: Read-only BUAA academic data via `iclass_buaa_tui`: 课表 (today, schedule export/diff), 考试, 成绩/GPA, 空教室, 作业. Load after `buaa-campus` when the user asks what classes, exams, grades, free classrooms, or assignments they have.
---

# Academics (all read-only)

Every command here only reads. No `--yes` is ever needed. Pass `--json` and parse stdout.

| question | command |
|---|---|
| 今天有什么课 / 下一节课 | `today --json` |
| 考试安排 | `exams --json [--term 2025-2026-1]` |
| 本学期成绩 | `grades --json [--term ...]` |
| 全部成绩 + 加权 GPA | `grades --all --json` |
| 空教室 | `classrooms --campus <1\|2\|3> [--date YYYY-MM-DD] [--section N] --json` |
| 作业 / 待交任务 | `tasks --json` |
| 导出课表 | `schedule-export --format <markdown\|csv\|json\|ics> [--term ...] [--output file]` |
| 两学期课表差异 | `schedule-diff [--from term] [--to term]` |

Campus ids: `1` 学院路, `2` 沙河, `3` 杭州. Ask which campus if the user has not said and it is not in context.

## The schedule cache

`today`, `exams`, and `grades` log in and import the term's schedule into a local cache when it is missing. `schedule-export` and `schedule-diff` read only that cache and never log in. When either reports no cached term, run `today --json` once to populate it, then retry. `schedule-diff` needs at least two cached terms.

For a calendar file the user can import into their calendar app, use `schedule-export --format ics --output <path>` and tell them the path.

## Presenting results

Term codes look like `2025-2026-1` (first semester of the 2025-2026 year). Omit `--term` for the current one. When summarising grades, keep the numbers as the tool reported them; the GPA from `grades --all` is credit-weighted and is the figure to quote.
