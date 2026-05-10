//! Built-in skill embedding and runtime expansion.
//!
//! At build time, `build.rs` copies fixed skill sources (`SKILL.md`, `overview.md`)
//! and all `docs/` files into `$OUT_DIR/builtin-skills/`, which is then embedded
//! into the binary via `include_dir`.
//!
//! At runtime, `expand_builtin_skills` writes the embedded tree to
//! `state_root/skills/egopulse/`, overwriting matching files from the prior version.

use std::fs;
use std::io;
use std::path::Path;

include!(concat!(env!("OUT_DIR"), "/builtin_skills.rs"));

/// Expands the embedded `egopulse` built-in skill to `state_root/skills/egopulse/`.
///
/// Matching files are overwritten so that binary updates provide the latest
/// embedded documentation. User skills under `state_root/workspace/skills/egopulse/` are
/// unaffected and take priority in [`SkillManager`](crate::skills::SkillManager).
pub(crate) fn expand_builtin_skills(state_root: &Path) -> io::Result<()> {
    let egopulse_dir = BUILTIN_SKILLS.get_dir("egopulse").expect(
        "builtin-skills/egopulse directory must exist in the embedded binary — \
         check build.rs",
    );
    let target = state_root.join("skills").join("egopulse");
    fs::create_dir_all(&target)?;
    extract_dir(egopulse_dir, &target)
}

fn extract_dir(dir: &include_dir::Dir<'_>, target: &Path) -> io::Result<()> {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(d) => {
                let dir_name = d.path().file_name().expect("embedded dir must have a name");
                let child_target = target.join(dir_name);
                fs::create_dir_all(&child_target)?;
                extract_dir(d, &child_target)?;
            }
            include_dir::DirEntry::File(f) => {
                let file_name = f
                    .path()
                    .file_name()
                    .expect("embedded file must have a name");
                fs::write(target.join(file_name), f.contents())?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_egopulse_skill_contains_skill_md() {
        let dir = BUILTIN_SKILLS
            .get_dir("egopulse")
            .expect("egopulse dir must exist");
        let has_skill_md = dir.entries().iter().any(|e| {
            matches!(
                e,
                include_dir::DirEntry::File(f) if f.path().file_name().is_some_and(|n| n == "SKILL.md")
            )
        });
        assert!(has_skill_md, "egopulse/SKILL.md must be embedded");
    }

    #[test]
    fn embedded_egopulse_skill_contains_overview() {
        let dir = BUILTIN_SKILLS
            .get_dir("egopulse")
            .expect("egopulse dir must exist");
        let has_references = dir.entries().iter().any(|e| {
            matches!(
                e,
                include_dir::DirEntry::Dir(d) if d.path().file_name().is_some_and(|n| n == "references")
            )
        });
        assert!(
            has_references,
            "egopulse/references directory must be embedded from docs/"
        );
    }

    #[test]
    fn embedded_references_include_docs_files() {
        let dir = BUILTIN_SKILLS
            .get_dir("egopulse")
            .expect("egopulse dir must exist");
        let refs_dir = dir
            .entries()
            .iter()
            .find_map(|e| match e {
                include_dir::DirEntry::Dir(d)
                    if d.path().file_name().is_some_and(|n| n == "references") =>
                {
                    Some(d)
                }
                _ => None,
            })
            .expect("references dir");
        let has_arch = refs_dir.entries().iter().any(|e| {
            matches!(
                e,
                include_dir::DirEntry::File(f) if f.path().file_name().is_some_and(|n| n == "architecture.md")
            )
        });
        assert!(
            has_arch,
            "references/architecture.md must be embedded from docs/"
        );
    }

    #[test]
    fn expand_writes_skill_md_and_references() {
        let tmp = tempfile::tempdir().expect("tempdir");
        expand_builtin_skills(tmp.path()).expect("expand");

        let skill_md = tmp.path().join("skills").join("egopulse").join("SKILL.md");
        assert!(skill_md.exists(), "SKILL.md should be written");
        let content = fs::read_to_string(&skill_md).expect("read SKILL.md");
        assert!(content.contains("name: egopulse"));

        let architecture = tmp
            .path()
            .join("skills")
            .join("egopulse")
            .join("references")
            .join("architecture.md");
        assert!(
            architecture.exists(),
            "references/architecture.md should be written from docs/"
        );
    }

    #[test]
    fn expand_overwrites_existing_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let skill_md = tmp.path().join("skills").join("egopulse").join("SKILL.md");
        fs::create_dir_all(skill_md.parent().unwrap()).expect("create dir");
        fs::write(&skill_md, "old content").expect("write old");

        expand_builtin_skills(tmp.path()).expect("expand");

        let content = fs::read_to_string(&skill_md).expect("read");
        assert_ne!(content, "old content");
        assert!(content.contains("name: egopulse"));
    }
}
