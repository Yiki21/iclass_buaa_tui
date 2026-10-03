//! `skills`: the Agent Skills that teach an LLM to drive this CLI.
//!
//! Why:
//! The skills describe this binary's exact command surface, so they ship
//! inside it. A caller that has the binary has skills matching its version,
//! and `skills install` drops them into whichever agent's skills directory the
//! user names.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::json;

use super::args::{SkillsAction, SkillsArgs};

/// One bundled file: skill-relative path and contents.

struct BundledFile {
    path:     &'static str,
    contents: &'static str,
}

macro_rules! bundled {
    ($($path:literal),* $(,)?) => {
        &[$(BundledFile {
            path:     $path,
            contents: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/skills/", $path)),
        }),*]
    };
}

/// Every file under `skills/`. Adding a file there means adding it here; the
/// test below fails if a skill directory has no SKILL.md entry.

const FILES: &[BundledFile] = bundled![
    "buaa-campus/SKILL.md",
    "buaa-academics/SKILL.md",
    "buaa-attendance/SKILL.md",
    "buaa-bykc/SKILL.md",
    "buaa-booking/SKILL.md",
    "buaa-clockin-eval/SKILL.md",
];

/// Marker written into installed files so a later install can tell its own
/// files from ones the user wrote.

const MARKER: &str = "<!-- installed by iclass_buaa_tui skills install -->";

pub(crate) fn skills_command(args: SkillsArgs) -> Result<()> {

    match args.action {
        SkillsAction::List { json } => list(json),
        SkillsAction::Show { name } => show(&name),
        SkillsAction::Install {
            target,
            force,
            yes,
            json,
        } => install(&target, force, yes, json),
    }
}

/// Reads one frontmatter field from a SKILL.md.

fn frontmatter(contents: &str, key: &str) -> Option<String> {

    let body = contents.strip_prefix("---\n")?;

    let end = body.find("\n---")?;

    body[..end].lines().find_map(|line| {

        line.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(':'))
            .map(|value| value.trim().to_string())
    })
}

fn skill_entries() -> impl Iterator<Item = &'static BundledFile> {

    FILES.iter().filter(|file| file.path.ends_with("/SKILL.md"))
}

fn list(as_json: bool) -> Result<()> {

    let skills: Vec<_> = skill_entries()
        .map(|file| {

            json!({
                "name": frontmatter(file.contents, "name"),
                "description": frontmatter(file.contents, "description"),
                "path": file.path,
            })
        })
        .collect();

    if as_json {

        println!("{}", serde_json::to_string_pretty(&skills)?);

        return Ok(());
    }

    for skill in &skills {

        println!(
            "{}\t{}",
            skill["name"].as_str().unwrap_or_default(),
            skill["description"].as_str().unwrap_or_default()
        );
    }

    Ok(())
}

/// Resolves `name` (a skill name or a skill-relative path) to a bundled file.

fn find(name: &str) -> Option<&'static BundledFile> {

    let name = name.trim().trim_end_matches('/');

    FILES.iter().find(|file| file.path == name).or_else(|| {

        FILES
            .iter()
            .find(|file| file.path == format!("{name}/SKILL.md"))
    })
}

fn show(name: &str) -> Result<()> {

    let Some(file) = find(name) else {

        let known: Vec<_> = skill_entries()
            .filter_map(|file| file.path.strip_suffix("/SKILL.md"))
            .collect();

        bail!("没有名为 {name} 的 skill。可用: {}", known.join(", "));
    };

    print!("{}", file.contents);

    Ok(())
}

/// What installing one file would do.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]

enum Plan {
    Create,
    Update,
    Unchanged,
    /// Exists but was not written by us, or differs and `--force` is absent.
    Conflict,
}

impl Plan {
    fn as_str(self) -> &'static str {

        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Unchanged => "unchanged",
            Self::Conflict => "conflict",
        }
    }
}

fn installed_contents(file: &BundledFile) -> String {

    // The marker goes after the frontmatter: agents require `---` on line 1.
    match file
        .contents
        .strip_prefix("---\n")
        .and_then(|rest| rest.find("\n---\n").map(|end| end + 4 + 5))
    {
        Some(split) => {

            format!(
                "{}{MARKER}\n{}",
                &file.contents[..split],
                &file.contents[split..]
            )
        }
        None => format!("{MARKER}\n{}", file.contents),
    }
}

fn plan_file(dest: &Path, wanted: &str, force: bool) -> Result<Plan> {

    if !dest.exists() {

        return Ok(Plan::Create);
    }

    let existing =
        fs::read_to_string(dest).with_context(|| format!("读取 {} 失败", dest.display()))?;

    if existing == wanted {

        return Ok(Plan::Unchanged);
    }

    // Overwrite only our own earlier output, and only when asked to: a user
    // may have edited the file on purpose.
    if existing.contains(MARKER) && force {

        Ok(Plan::Update)
    } else {

        Ok(Plan::Conflict)
    }
}

fn install(target: &Path, force: bool, yes: bool, as_json: bool) -> Result<()> {

    let mut planned: Vec<(PathBuf, String, Plan)> = Vec::new();

    for file in FILES {

        let dest = target.join(file.path);

        let wanted = installed_contents(file);

        let plan = plan_file(&dest, &wanted, force)?;

        planned.push((dest, wanted, plan));
    }

    let conflicts: Vec<_> = planned
        .iter()
        .filter(|(_, _, plan)| *plan == Plan::Conflict)
        .map(|(dest, _, _)| dest.display().to_string())
        .collect();

    let report = |submitted: bool| {

        json!({
            "action": "skills install",
            "submitted": submitted,
            "target": target.display().to_string(),
            "files": planned
                .iter()
                .map(|(dest, _, plan)| json!({"path": dest.display().to_string(), "plan": plan.as_str()}))
                .collect::<Vec<_>>(),
        })
    };

    if !yes {

        if as_json {

            let mut preview = report(false);

            preview["hint"] = json!("加上 --yes 才会写入");

            println!("{}", serde_json::to_string_pretty(&preview)?);
        } else {

            for (dest, _, plan) in &planned {

                println!("{}\t{}", plan.as_str(), dest.display());
            }

            println!("这是预览。加上 --yes 才会写入。");
        }

        return Ok(());
    }

    // All-or-nothing: refuse before writing anything rather than leave a
    // half-updated set of skills that disagree with each other.
    if !conflicts.is_empty() {

        bail!(
            "以下文件已存在且内容不同，未写入任何文件（我们自己装过的文件可加 --force 覆盖）:\n{}",
            conflicts.join("\n")
        );
    }

    for (dest, wanted, plan) in &planned {

        if matches!(plan, Plan::Create | Plan::Update) {

            if let Some(parent) = dest.parent() {

                fs::create_dir_all(parent)
                    .with_context(|| format!("创建 {} 失败", parent.display()))?;
            }

            fs::write(dest, wanted).with_context(|| format!("写入 {} 失败", dest.display()))?;
        }
    }

    if as_json {

        println!("{}", serde_json::to_string_pretty(&report(true))?);
    } else {

        for (dest, _, plan) in &planned {

            println!("{}\t{}", plan.as_str(), dest.display());
        }
    }

    Ok(())
}

#[cfg(test)]

mod tests {

    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;

    use super::{
        FILES, MARKER, Plan, find, frontmatter, installed_contents, plan_file, skill_entries,
    };
    use crate::cli::schema::schema_json;

    fn scratch(name: &str) -> PathBuf {

        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".tmp")
            .join(format!("skills-test-{name}-{}", std::process::id()));

        let _ = fs::remove_dir_all(&dir);

        fs::create_dir_all(&dir).expect("应能创建临时目录");

        dir
    }

    #[test]

    fn every_skill_directory_is_bundled() {

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills");

        let on_disk: BTreeSet<String> = fs::read_dir(&root)
            .expect("应有 skills 目录")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().join("SKILL.md").is_file())
            .map(|entry| format!("{}/SKILL.md", entry.file_name().to_string_lossy()))
            .collect();

        let bundled: BTreeSet<String> = FILES.iter().map(|file| file.path.to_string()).collect();

        assert_eq!(on_disk, bundled, "skills/ 与 FILES 不一致");
    }

    #[test]

    fn every_skill_has_a_name_matching_its_directory_and_a_description() {

        for file in skill_entries() {

            let dir = file.path.trim_end_matches("/SKILL.md");

            assert_eq!(frontmatter(file.contents, "name").as_deref(), Some(dir));

            let description = frontmatter(file.contents, "description").unwrap_or_default();

            assert!(
                !description.is_empty() && description.len() <= 1024,
                "{dir} description"
            );
        }
    }

    #[test]

    fn every_command_a_skill_mentions_exists() {

        // Skills name commands in backticks or after the binary name. Every
        // such name must be a real command, or an agent will call one that
        // does not exist.
        let schema = schema_json().expect("应能生成 schema");

        let commands: BTreeSet<String> = schema["commands"]
            .as_array()
            .expect("应有 commands")
            .iter()
            .filter_map(|entry| entry["name"].as_str())
            .flat_map(|name| {

                [
                    name.to_string(),
                    name.split(' ').next().unwrap_or(name).to_string(),
                ]
            })
            .collect();

        for file in FILES {

            for line in file.contents.lines() {

                if let Some(rest) = line.split("iclass_buaa_tui ").nth(1) {

                    let word = rest.split_whitespace().next().unwrap_or_default();

                    if !word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                    {

                        assert!(commands.contains(word), "{}: 未知命令 {word}", file.path);
                    }
                }
            }
        }

        // And every command is covered by some skill.
        let all_text: String = FILES.iter().map(|file| file.contents).collect();

        for name in &commands {

            if name.contains(' ') {

                continue;
            }

            assert!(
                all_text.contains(&format!("`{name}")),
                "没有 skill 提到 {name}"
            );
        }
    }

    #[test]

    fn find_accepts_a_name_or_a_path() {

        assert!(find("buaa-campus").is_some());

        assert!(find("buaa-booking/SKILL.md").is_some());

        assert!(find("nope").is_none());
    }

    #[test]

    fn installed_files_keep_frontmatter_on_the_first_line() {

        for file in FILES {

            let installed = installed_contents(file);

            assert!(installed.starts_with("---\n"), "{}", file.path);

            assert!(installed.contains(MARKER));

            assert_eq!(installed.replace(&format!("{MARKER}\n"), ""), file.contents);
        }
    }

    #[test]

    fn user_files_are_never_overwritten_and_ours_only_with_force() {

        let dir = scratch("plan");

        let dest = dir.join("SKILL.md");

        assert_eq!(plan_file(&dest, "new", false).unwrap(), Plan::Create);

        fs::write(&dest, "new").unwrap();

        assert_eq!(plan_file(&dest, "new", false).unwrap(), Plan::Unchanged);

        fs::write(&dest, "written by the user").unwrap();

        assert_eq!(plan_file(&dest, "new", true).unwrap(), Plan::Conflict);

        fs::write(&dest, format!("old\n{MARKER}\n")).unwrap();

        assert_eq!(plan_file(&dest, "new", false).unwrap(), Plan::Conflict);

        assert_eq!(plan_file(&dest, "new", true).unwrap(), Plan::Update);

        let _ = fs::remove_dir_all(&dir);
    }
}
