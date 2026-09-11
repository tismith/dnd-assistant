use dnd_assistant_core::WorkspaceDocument;
use std::{
    fs,
    path::{Path, PathBuf},
};

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_DOCUMENTS: usize = 2_000;
const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;

/// Local, read-only Markdown workspace indexed for agent context lookup.
#[derive(Debug, Default)]
pub struct Workspace {
    documents: Vec<WorkspaceDocument>,
}

impl Workspace {
    pub fn load(paths: &[String]) -> Self {
        let mut workspace = Self::default();
        let paths = if paths.is_empty() {
            vec![std::env::current_dir().unwrap_or_else(|error| {
                eprintln!("cannot determine current workspace directory: {error}");
                Path::new(".").to_path_buf()
            })]
        } else {
            paths.iter().map(PathBuf::from).collect()
        };
        for configured in paths {
            let path = configured.as_path();
            if path.is_dir() {
                workspace.collect_directory(path);
            } else if path.is_file() {
                workspace.read_file(path);
            } else {
                eprintln!(
                    "workspace path does not exist; skipping: {}",
                    configured.display()
                );
            }
        }
        workspace
    }

    pub fn all(&self) -> Vec<WorkspaceDocument> {
        self.documents.clone()
    }

    #[allow(dead_code)]
    pub fn list(&self, prefix: Option<&str>) -> Vec<String> {
        self.documents
            .iter()
            .filter(|document| prefix.is_none_or(|prefix| document.path.starts_with(prefix)))
            .map(|document| document.path.clone())
            .collect()
    }

    #[allow(dead_code)]
    pub fn read(&self, path: &str) -> Option<WorkspaceDocument> {
        self.documents
            .iter()
            .find(|document| document.path == path)
            .cloned()
    }

    /// Return the most relevant documents for a natural-language query.
    /// Scores use lexical matches in both the path and Markdown content.
    pub fn search(&self, query: &str, limit: usize) -> Vec<WorkspaceDocument> {
        let terms = terms(query);
        if terms.is_empty() {
            return self.documents.iter().take(limit).cloned().collect();
        }
        let mut ranked = self
            .documents
            .iter()
            .filter_map(|document| {
                let score = score(document, &terms);
                (score > 0).then_some((score, document))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.path.cmp(&right.path))
        });
        ranked
            .into_iter()
            .take(limit)
            .map(|(_, document)| document.clone())
            .collect()
    }

    fn collect_directory(&mut self, directory: &Path) {
        if self.documents.len() >= MAX_DOCUMENTS {
            return;
        }
        let Ok(entries) = fs::read_dir(directory) else {
            eprintln!("cannot read workspace directory: {}", directory.display());
            return;
        };
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            if self.documents.len() >= MAX_DOCUMENTS {
                break;
            }
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                continue;
            }
            if path.is_dir() {
                self.collect_directory(&path);
            } else if path.extension().is_some_and(|extension| extension == "md") {
                self.read_file(&path);
            }
        }
    }

    fn read_file(&mut self, path: &Path) {
        if self.documents.len() >= MAX_DOCUMENTS
            || self
                .documents
                .iter()
                .map(|document| document.content.len())
                .sum::<usize>()
                >= MAX_TOTAL_BYTES
        {
            return;
        }
        let Ok(metadata) = fs::metadata(path) else {
            eprintln!("cannot inspect workspace file: {}", path.display());
            return;
        };
        let total = self
            .documents
            .iter()
            .map(|document| document.content.len())
            .sum::<usize>();
        let remaining = MAX_TOTAL_BYTES.saturating_sub(total);
        if metadata.len() > MAX_FILE_BYTES || metadata.len() as usize > remaining {
            eprintln!(
                "workspace file exceeds context limits; skipping: {}",
                path.display()
            );
            return;
        }
        if let Ok(content) = fs::read_to_string(path) {
            self.documents.push(WorkspaceDocument {
                path: path.display().to_string(),
                content,
            });
        }
    }
}

fn terms(query: &str) -> Vec<String> {
    query
        .split(|character: char| !character.is_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|term| term.len() >= 2)
        .collect()
}

fn score(document: &WorkspaceDocument, terms: &[String]) -> usize {
    let path = document.path.to_ascii_lowercase();
    let content = document.content.to_ascii_lowercase();
    terms
        .iter()
        .map(|term| path.matches(term).count() * 5 + content.matches(term).count())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::Workspace;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn indexes_lists_reads_and_searches_markdown() {
        let root = std::env::temp_dir().join(format!(
            "dnd-workspace-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("npcs")).unwrap();
        fs::write(
            root.join("npcs/voss.md"),
            "Captain Voss wants the Wind Bell.",
        )
        .unwrap();
        fs::write(root.join("notes.txt"), "not indexed").unwrap();
        let workspace = Workspace::load(&[root.display().to_string()]);
        let path = root.join("npcs/voss.md").display().to_string();
        assert_eq!(workspace.list(None), vec![path.clone()]);
        assert_eq!(
            workspace.read(&path).unwrap().content,
            "Captain Voss wants the Wind Bell."
        );
        assert_eq!(workspace.search("wind bell", 1)[0].path, path);
        let _ = fs::remove_dir_all(root);
    }
}
