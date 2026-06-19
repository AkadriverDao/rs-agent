use std::sync::Mutex;

use anyhow::{Context, Result};
use git2::{DiffOptions, Repository, Signature, StatusOptions};

pub struct GitManager {
    repo: Mutex<Repository>,
    pub workdir: String,
}

impl GitManager {
    pub fn open(path: &str) -> Result<Self> {
        let repo = if Self::is_repo(path) {
            Repository::open(path).context("Failed to open git repo")?
        } else {
            Repository::init(path).context("Failed to init git repo")?
        };
        let workdir = repo
            .workdir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string());
        Ok(Self {
            repo: Mutex::new(repo),
            workdir,
        })
    }

    pub fn is_repo(path: &str) -> bool {
        Repository::open(path).is_ok()
    }

    pub fn auto_commit(&self, message: &str) -> Result<String> {
        let repo = self.repo.lock().unwrap();
        let mut index = repo.index()?;
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
        index.write()?;

        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;

        let sig = signature()?;
        let parent = last_commit(&repo).ok();

        let commit_id = if let Some(parent) = &parent {
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[parent])?
        } else {
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[])?
        };

        Ok(commit_id.to_string())
    }

    pub fn diff_uncommitted(&self) -> Result<String> {
        let repo = self.repo.lock().unwrap();
        let head = last_commit(&repo);
        let tree = match &head {
            Ok(c) => c.tree().ok(),
            Err(_) => None,
        };

        let mut opts = DiffOptions::new();
        opts.context_lines(2);

        let diff = repo
            .diff_tree_to_workdir(tree.as_ref(), Some(&mut opts))
            .context("Failed to get diff")?;

        format_diff(&diff)
    }

    pub fn status(&self) -> Result<Vec<String>> {
        let repo = self.repo.lock().unwrap();
        let mut opts = StatusOptions::new();
        opts.include_untracked(true);

        let statuses = repo.statuses(Some(&mut opts))?;

        let mut result = Vec::new();
        for entry in statuses.iter() {
            let path = entry.path().unwrap_or("?");
            let flags = entry.status();
            let label = if flags.contains(git2::Status::CURRENT) {
                continue;
            } else if flags.contains(git2::Status::INDEX_NEW) || flags.contains(git2::Status::WT_NEW) {
                format!("+ {}", path)
            } else if flags.contains(git2::Status::INDEX_MODIFIED) || flags.contains(git2::Status::WT_MODIFIED) {
                format!("~ {}", path)
            } else if flags.contains(git2::Status::INDEX_DELETED) || flags.contains(git2::Status::WT_DELETED) {
                format!("- {}", path)
            } else {
                format!("? {}", path)
            };
            result.push(label);
        }
        Ok(result)
    }

    pub fn undo(&self) -> Result<String> {
        let repo = self.repo.lock().unwrap();
        let head = last_commit(&repo)?;
        let parent = head.parents().next();
        match parent {
            Some(p) => {
                let obj = repo.find_object(p.id(), None)?;
                repo.reset(&obj, git2::ResetType::Mixed, None)?;
                Ok(format!("reset to {}", p.id().to_string()))
            }
            None => {
                let zero = git2::Oid::ZERO_SHA1;
                let obj = repo.find_object(zero, None)?;
                repo.reset(&obj, git2::ResetType::Mixed, None)?;
                Ok("reset to initial state".to_string())
            }
        }
    }
}

fn last_commit<'a>(repo: &'a Repository) -> Result<git2::Commit<'a>> {
    let head = repo.head()?;
    Ok(head.peel_to_commit()?)
}

fn signature() -> Result<Signature<'static>> {
    Ok(Signature::now("agent-engine", "agent@engine.local")?)
}

fn format_diff(diff: &git2::Diff) -> Result<String> {
    let mut files = Vec::new();
    let mut hunks = Vec::new();

    diff.foreach(
        &mut |delta, _| {
            let status = delta.status();
            let file = delta.new_file().path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            let prefix = match status {
                git2::Delta::Added => "+",
                git2::Delta::Deleted => "-",
                git2::Delta::Modified => "~",
                _ => " ",
            };
            files.push(format!("{} {}", prefix, file));
            true
        },
        None,
        None,
        Some(&mut |_delta, _before, after| {
            if let Ok(line) = std::str::from_utf8(after.content()) {
                let ch = after.origin() as char;
                if ch == '+' || ch == '-' {
                    hunks.push(format!("{}{}", ch, line.trim_end()));
                }
            }
            true
        }),
    )?;

    let mut output = String::new();
    for f in &files {
        output.push_str(f);
        output.push('\n');
    }
    for h in &hunks {
        output.push_str(h);
        output.push('\n');
    }
    Ok(output)
}
