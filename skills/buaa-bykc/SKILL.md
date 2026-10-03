---
name: buaa-bykc
description: Browse and enroll in BUAA 博雅课程 (BYKC) via `iclass_buaa_tui`. Load after `buaa-campus` when the user asks which 博雅 courses are open, wants to 选课 or 退选, check their chosen courses, or see how many 博雅 credits per category they still need.
---

# 博雅课程 (BYKC)

BYKC is reached through WebVPN. Every command here needs `use_vpn = true` in the config; without it they fail with a message saying so. Tell the user to set it; do not edit their config yourself.

## Reads

| question | command |
|---|---|
| 现在能报哪些课 | `bykc-courses --json` |
| 包括已满/已截止的 | `bykc-courses --all --json` |
| 我选了哪些 | `bykc-chosen --json` |
| 一门课的详情、签到窗口 | `bykc-detail --course <id> --json` |
| 各类别还差几门 | `bykc-stats --json` |

Ids: in `bykc-courses` and `bykc-detail` the course id is `id`. In `bykc-chosen` use `course_id`; its `id` is the enrollment record, not the course.

`bykc-stats` returns `total_valid_count` and `categories` with `category_name`, `sub_category`, `required_count`, `passed_count`, `is_qualified`. To suggest courses, match an open course's `category` / `sub_category` against the `category_name` / `sub_category` of entries where `is_qualified` is false.

## Enroll and withdraw

```bash
iclass_buaa_tui bykc-select   --course <id> --json        # preview
iclass_buaa_tui bykc-select   --course <id> --yes --json  # enroll
iclass_buaa_tui bykc-deselect --course <id> --yes --json  # withdraw
```

The preview re-reads the course and fails early when the write would be pointless (already enrolled, or not enrolled). It reports `count` / `max`, `select_end`, and `cancel_end`.

Before `--yes`, tell the user the course name, start time, and location, and get an explicit yes for that course. Withdrawal is only possible until `cancel_end`; after it an enrollment is effectively permanent, and missing an enrolled course can count against the user. Mention `cancel_end` when enrolling.

A full course or closed window comes back as `resource_unavailable`. Re-list and offer another course.

## Signing in to a 博雅 course

Attendance for an enrolled course is `sign --source bykc`; see `buaa-attendance`. The window is in `bykc-detail` under `sign_config`.
