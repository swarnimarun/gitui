//! sync git api for fetching a diff

use super::utils;
use git2::{
    Delta, DiffDelta, DiffFormat, DiffHunk, DiffOptions, Patch,
};
use scopetime::scope_time;
use std::{fs, path::Path};

///
#[derive(Copy, Clone, PartialEq, Hash)]
pub enum DiffLineType {
    ///
    None,
    ///
    Header,
    ///
    Add,
    ///
    Delete,
}

impl Default for DiffLineType {
    fn default() -> Self {
        DiffLineType::None
    }
}

///
#[derive(Default, Clone, Hash)]
pub struct DiffLine {
    ///
    pub content: String,
    ///
    pub line_type: DiffLineType,
}

///
#[derive(Default, Clone, Copy, PartialEq)]
struct HunkHeader {
    old_start: u32,
    old_lines: u32,
    new_start: u32,
    new_lines: u32,
}

impl From<DiffHunk<'_>> for HunkHeader {
    fn from(h: DiffHunk) -> Self {
        Self {
            old_start: h.old_start(),
            old_lines: h.old_lines(),
            new_start: h.new_start(),
            new_lines: h.new_lines(),
        }
    }
}

///
#[derive(Default, Clone, Hash)]
pub struct Hunk(pub Vec<DiffLine>);

///
#[derive(Default, Clone, Hash)]
pub struct Diff(pub Vec<Hunk>, pub u16);

///
pub fn get_diff(repo_path: &str, p: String, stage: bool) -> Diff {
    scope_time!("get_diff");

    let repo = utils::repo(repo_path);

    let mut opt = DiffOptions::new();
    opt.pathspec(p);

    let diff = if stage {
        // diff against head
        let ref_head = repo.head().unwrap();
        let parent =
            repo.find_commit(ref_head.target().unwrap()).unwrap();
        let tree = parent.tree().unwrap();
        repo.diff_tree_to_index(
            Some(&tree),
            Some(&repo.index().unwrap()),
            Some(&mut opt),
        )
        .unwrap()
    } else {
        opt.include_untracked(true);
        opt.recurse_untracked_dirs(true);
        repo.diff_index_to_workdir(None, Some(&mut opt)).unwrap()
    };

    let mut res: Diff = Diff::default();
    let mut current_lines = Vec::new();
    let mut current_hunk: Option<HunkHeader> = None;

    let mut adder = |lines: &Vec<DiffLine>| {
        res.0.push(Hunk(lines.clone()));
        res.1 += lines.len() as u16;
    };

    let mut put = |hunk: Option<DiffHunk>, line: git2::DiffLine| {
        if let Some(hunk) = hunk {
            let hunk_header = HunkHeader::from(hunk);

            match current_hunk {
                None => current_hunk = Some(hunk_header),
                Some(h) if h != hunk_header => {
                    adder(&current_lines);
                    current_lines.clear();
                    current_hunk = Some(hunk_header)
                }
                _ => (),
            }

            let line_type = match line.origin() {
                'H' => DiffLineType::Header,
                '<' | '-' => DiffLineType::Delete,
                '>' | '+' => DiffLineType::Add,
                _ => DiffLineType::None,
            };

            let diff_line = DiffLine {
                content: String::from_utf8_lossy(line.content())
                    .to_string(),
                line_type,
            };

            current_lines.push(diff_line);
        }
    };

    let new_file_diff = if diff.deltas().len() == 1 {
        let delta: DiffDelta = diff.deltas().next().unwrap();

        if delta.status() == Delta::Untracked {
            let repo_path = Path::new(repo_path);
            let newfile_path =
                repo_path.join(delta.new_file().path().unwrap());

            let newfile_content = new_file_content(&newfile_path);

            let mut patch = Patch::from_buffers(
                &[],
                None,
                newfile_content.as_bytes(),
                Some(&newfile_path),
                Some(&mut opt),
            )
            .unwrap();

            patch
                .print(&mut |_delta, hunk:Option<DiffHunk>, line: git2::DiffLine| {
                    put(hunk,line);
                    true
                })
                .unwrap();

            true
        } else {
            false
        }
    } else {
        false
    };

    if !new_file_diff {
        diff.print(
            DiffFormat::Patch,
            |_, hunk, line: git2::DiffLine| {
                put(hunk, line);
                true
            },
        )
        .unwrap();
    }

    if !current_lines.is_empty() {
        adder(&current_lines);
    }

    res
}

fn new_file_content(path: &Path) -> String {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return fs::read_link(path)
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
        } else if meta.file_type().is_file() {
            if let Ok(content) = fs::read_to_string(path) {
                return content;
            }
        }
    }

    "file not found".to_string()
}

#[cfg(test)]
mod tests {
    use super::get_diff;
    use crate::sync::{
        stage_add,
        status::{get_status, StatusType},
        tests::repo_init,
    };
    use std::{
        fs::{self, File},
        io::Write,
        path::Path,
    };

    #[test]
    fn test_untracked_subfolder() {
        let (_td, repo) = repo_init();
        let root = repo.path().parent().unwrap();
        let repo_path = root.as_os_str().to_str().unwrap();

        let res = get_status(repo_path, StatusType::WorkingDir);
        assert_eq!(res.len(), 0);

        fs::create_dir(&root.join("foo")).unwrap();
        File::create(&root.join("foo/bar.txt"))
            .unwrap()
            .write_all(b"test\nfoo")
            .unwrap();

        let res = get_status(repo_path, StatusType::WorkingDir);
        assert_eq!(res.len(), 1);

        let diff =
            get_diff(repo_path, "foo/bar.txt".to_string(), false);

        assert_eq!(diff.0.len(), 1);
        assert_eq!(diff.0[0].0[1].content, "test\n");
    }

    static HUNK_A: &str = r"
1   start
2
3
4
5
6   middle
7
8
9
0
1   end";

    static HUNK_B: &str = r"
1   start
2   newa
3
4
5
6   middle
7
8
9
0   newb
1   end";

    #[test]
    fn test_hunks() {
        let (_td, repo) = repo_init();
        let root = repo.path().parent().unwrap();
        let repo_path = root.as_os_str().to_str().unwrap();

        let res = get_status(repo_path, StatusType::WorkingDir);
        assert_eq!(res.len(), 0);

        let file_path = root.join("bar.txt");

        {
            File::create(&file_path)
                .unwrap()
                .write_all(HUNK_A.as_bytes())
                .unwrap();
        }

        let res = get_status(repo_path, StatusType::WorkingDir);
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].path, "bar.txt");

        let res = stage_add(repo_path, Path::new("bar.txt"));
        assert_eq!(res, true);
        assert_eq!(get_status(repo_path, StatusType::Stage).len(), 1);
        assert_eq!(
            get_status(repo_path, StatusType::WorkingDir).len(),
            0
        );

        // overwrite with next content
        {
            File::create(&file_path)
                .unwrap()
                .write_all(HUNK_B.as_bytes())
                .unwrap();
        }

        assert_eq!(get_status(repo_path, StatusType::Stage).len(), 1);
        assert_eq!(
            get_status(repo_path, StatusType::WorkingDir).len(),
            1
        );

        let res = get_diff(repo_path, "bar.txt".to_string(), false);

        assert_eq!(res.0.len(), 2)
    }
}
