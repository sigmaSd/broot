use {
    super::{
        bid::{BId, SortableBId},
        bline::BLine,
    },
    crate::{
        app::AppContext,
        errors::TreeBuildError,
        git::{GitIgnoreChain, GitIgnorer, LineStatusComputer},
        pattern::Candidate,
        path::{SpecialHandling, SpecialPathList},
        task_sync::ComputationResult,
        task_sync::Dam,
        tree::*,
    },
    git2::Repository,
    id_arena::Arena,
    rayon::prelude::*,
    std::{
        collections::{BinaryHeap, VecDeque},
        fs,
        path::PathBuf,
        result::Result,
        time::{Duration, Instant},
    },
};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

#[cfg(target_os = "windows")]
use std::ffi::OsStr;

#[cfg(target_os = "windows")]
trait OsStrWin {
    fn as_bytes(&self) -> &[u8];
}

#[cfg(target_os = "windows")]
impl OsStrWin for OsStr {
    fn as_bytes(&self) -> &[u8] {
        static INVALID_UTF8: &[u8] = b"invalid utf8";
        self.to_str().map(|s| s.as_bytes()).unwrap_or(INVALID_UTF8)
    }
}
/// If a search found enough results to fill the screen but didn't scan
/// everything, we search a little more in case we find better matches
/// but not after the NOT_LONG duration.
static NOT_LONG: Duration = Duration::from_millis(900);

/// The TreeBuilder builds a Tree according to options (including an optional search pattern)
/// Instead of the final TreeLine, the builder uses an internal structure: BLine.
/// All BLines used during build are stored in the blines arena and kept until the end.
/// Most operations and temporary data structures just deal with the ids of lines
///  the blines arena.
pub struct TreeBuilder<'c> {
    pub options: TreeOptions,
    targeted_size: usize, // the number of lines we should fill (height of the screen)
    nb_gitignored: u32,   // number of times a gitignore pattern excluded a file
    blines: Arena<BLine>,
    root_id: BId,
    total_search: bool,
    git_ignorer: GitIgnorer,
    line_status_computer: Option<LineStatusComputer>,
    con: &'c AppContext,
    trim_root: bool,
}
impl<'c> TreeBuilder<'c> {

    pub fn from(
        path: PathBuf,
        options: TreeOptions,
        targeted_size: usize,
        con: &'c AppContext,
    ) -> Result<TreeBuilder<'c>, TreeBuildError> {
        let mut blines = Arena::new();
        let mut git_ignorer = time!(GitIgnorer::default());
        let root_ignore_chain = git_ignorer.root_chain(&path);
        let line_status_computer = if options.filter_by_git_status || options.show_git_file_info {
            time!(
                "init line_status_computer",
                Repository::discover(&path)
                    .ok()
                    .map(LineStatusComputer::from),
            )
        } else {
            None
        };
        let root_id = BLine::from_root(&mut blines, path, root_ignore_chain, &options)?;
        let trim_root = options.pattern.is_some()
            || (options.trim_root && !options.sort.is_some());
        Ok(TreeBuilder {
            options,
            targeted_size,
            nb_gitignored: 0,
            blines,
            root_id,
            total_search: true, // we'll set it to false if we don't look at all children
            git_ignorer,
            line_status_computer,
            con,
            trim_root,
        })
    }

    /// return a bline if the dir_entry directly matches the options and there's no error
    fn make_line(
        &self,
        parent_id: BId,
        e: &fs::DirEntry,
        depth: u16,
    ) -> Option<BLine> {
        let name = e.file_name();
        if name.is_empty() {
            return None;
        }
        if !self.options.show_hidden && name.as_bytes()[0] == b'.' {
            return None;
        }
        let name = name.to_string_lossy();
        let mut has_match = true;
        let mut score = 10000 - i32::from(depth); // we dope less deep entries
        let path = e.path();
        let file_type = match e.file_type() {
            Ok(ft) => ft,
            Err(_) => {
                return None;
            }
        };
        let parent_subpath = &self.blines[parent_id].subpath;
        let subpath = if !parent_subpath.is_empty() {
            format!("{}/{}", parent_subpath, &name)
        } else {
            name.to_string()
        };
        let candidate = Candidate {
            name: &name,
            subpath: &subpath,
            path: &path,
            regular_file: file_type.is_file(),
        };
        let direct_match = if let Some(pattern_score) = self.options.pattern.pattern.score_of(candidate) {
            // we dope direct matchs to compensate for depth doping of parent folders
            score += pattern_score + 10;
            true
        } else {
            has_match = false;
            false
        };
        let name = name.to_string();
        if has_match && self.options.filter_by_git_status {
            if let Some(line_status_computer) = &self.line_status_computer {
                if !line_status_computer.is_interesting(&path) {
                    has_match = false;
                }
            }
        }
        if file_type.is_file() || file_type.is_symlink() {
            if !has_match {
                return None;
            }
            if self.options.only_folders {
                return None;
            }
        }
        let special_handling = self.con.special_paths.find(&path);
        if special_handling == SpecialHandling::Hide {
            return None;
        }
        if self.options.respect_git_ignore {
            let parent_chain = &self.blines[parent_id].git_ignore_chain;
            if !self
                .git_ignorer
                .accepts(parent_chain, &path, &name, file_type.is_dir())
            {
                return None;
            }
        };
        Some(BLine {
            parent_id: Some(parent_id),
            path,
            depth,
            subpath,
            name,
            file_type,
            children: None,
            next_child_idx: 0,
            has_error: false,
            has_match,
            direct_match,
            score,
            nb_kept_children: 0,
            git_ignore_chain: GitIgnoreChain::default(),
            special_handling,
        })
    }

    /// returns true when there are direct matches among children
    fn load_children(&mut self, bid: BId) -> bool {
        let mut has_child_match = false;
        match fs::read_dir(&self.blines[bid].path) {
            Ok(entries) => {
                let mut children: Vec<BId> = Vec::new();
                let child_depth = self.blines[bid].depth + 1;
                let entries: Vec<fs::DirEntry> = entries.filter_map(Result::ok).collect();
                let lines: Vec<BLine> = entries
                    .par_iter()
                    .filter_map(|e| self.make_line(bid, e, child_depth))
                    .collect();
                for mut bl in lines {
                    if self.options.respect_git_ignore {
                        let parent_chain = &self.blines[bid].git_ignore_chain;
                        bl.git_ignore_chain = if bl.file_type.is_dir() {
                            self.git_ignorer.deeper_chain(parent_chain, &bl.path)
                        } else {
                            parent_chain.clone()
                        };
                    }
                    if bl.has_match {
                        self.blines[bid].has_match = true;
                        has_child_match = true;
                    }
                    let child_id = self.blines.alloc(bl);
                    children.push(child_id);
                }
                children.sort_by(|&a, &b| {
                    self.blines[a]
                        .name
                        .to_lowercase()
                        .cmp(&self.blines[b].name.to_lowercase())
                });
                self.blines[bid].children = Some(children);
            }
            Err(_err) => {
                self.blines[bid].has_error = true;
                self.blines[bid].children = Some(Vec::new());
            }
        }
        has_child_match
    }

    /// return the next child.
    /// load_children must have been called before on parent_id
    fn next_child(&mut self, parent_id: BId) -> Option<BId> {
        let bline = &mut self.blines[parent_id];
        if let Some(children) = &bline.children {
            if bline.next_child_idx < children.len() {
                let next_child = children[bline.next_child_idx];
                bline.next_child_idx += 1;
                Some(next_child)
            } else {
                Option::None
            }
        } else {
            unreachable!();
        }
    }

    /// first step of the build: we explore the directories and gather lines.
    /// If there's no search pattern we stop when we have enough lines to fill the screen.
    /// If there's a pattern, we try to gather more lines that will be sorted afterwards.
    fn gather_lines(&mut self, total_search: bool, dam: &Dam) -> Option<Vec<BId>> {
        let start = Instant::now();
        let mut out_blines: Vec<BId> = Vec::new(); // the blines we want to display
        let optimal_size = if self.options.pattern.pattern.has_real_scores() {
            10 * self.targeted_size
        } else {
            self.targeted_size
        };
        out_blines.push(self.root_id);
        let mut nb_lines_ok = 1; // in out_blines
        let mut open_dirs: VecDeque<BId> = VecDeque::new();
        let mut next_level_dirs: Vec<BId> = Vec::new();
        self.load_children(self.root_id);
        open_dirs.push_back(self.root_id);


        let timer = std::time::Instant::now();
        let limit = std::env::var("BrootSearchLimit")
            .map(|limit| Some(Duration::from_millis(limit.parse().ok()?)))
            .ok()
            .flatten();

        loop {
            if !total_search && (
                (nb_lines_ok > optimal_size)
                || (nb_lines_ok >= self.targeted_size && start.elapsed() > NOT_LONG)
            ) {
                self.total_search = false;
                break;
            }
            if let Some(open_dir_id) = open_dirs.pop_front() {
                if let Some(child_id) = self.next_child(open_dir_id) {
                    open_dirs.push_back(open_dir_id);
                    let child = &self.blines[child_id];
                    if child.has_match {
                        nb_lines_ok += 1;
                    }
                    if child.can_enter() {
                        next_level_dirs.push(child_id);
                    }
                    out_blines.push(child_id);
                }
            } else {
                // this depth is finished, we must go deeper
                if self.options.sort.is_some() {
                    // in sort mode, only one level is displayed
                    break;
                }
                if next_level_dirs.is_empty() {
                    // except there's nothing deeper
                    break;
                }
                if let Some(limit) = limit {
                    if timer.elapsed() > limit {
                        // too much time has passed
                        break;
                    }
                }
                for next_level_dir_id in &next_level_dirs {
                    if dam.has_event() {
                        info!("task expired (core build - inner loop)");
                        return None;
                    }
                    let has_child_match = self.load_children(*next_level_dir_id);
                    if has_child_match {
                        // we must ensure the ancestors are made Ok
                        let mut id = *next_level_dir_id;
                        loop {
                            let mut bline = &mut self.blines[id];
                            if !bline.has_match {
                                bline.has_match = true;
                                nb_lines_ok += 1;
                            }
                            if let Some(pid) = bline.parent_id {
                                id = pid;
                            } else {
                                break;
                            }
                        }
                    }
                    open_dirs.push_back(*next_level_dir_id);
                }
                next_level_dirs.clear();
            }
        }
        if !self.trim_root {
            // if the root directory isn't totally read, we finished it even
            // it it goes past the bottom of the screen
            while let Some(child_id) = self.next_child(self.root_id) {
                out_blines.push(child_id);
            }
        }
        Some(out_blines)
    }

    /// Post search trimming
    /// When there's a pattern, gathering normally brings many more lines than
    ///  strictly necessary to fill the screen.
    /// This function keeps only the best ones while taking care of not
    ///  removing a parent before its children.
    fn trim_excess(&mut self, out_blines: &[BId]) {
        let mut count = 1;
        for id in out_blines[1..].iter() {
            if self.blines[*id].has_match {
                //debug!("bline before trimming: {:?}", &self.blines[*idx].path);
                count += 1;
                let parent_id = self.blines[*id].parent_id.unwrap();
                // (we can unwrap because only the root can have a None parent)
                self.blines[parent_id].nb_kept_children += 1;
            }
        }
        let mut remove_queue: BinaryHeap<SortableBId> = BinaryHeap::new();
        for id in out_blines[1..].iter() {
            let bline = &self.blines[*id];
            if bline.has_match && bline.nb_kept_children == 0 && (bline.depth > 1 || self.trim_root)
            {
                //debug!("in list: {:?} score: {}",  &bline.path, bline.score);
                remove_queue.push(SortableBId {
                    id: *id,
                    score: bline.score,
                });
            }
        }
        while count > self.targeted_size {
            if let Some(sli) = remove_queue.pop() {
                self.blines[sli.id].has_match = false;
                let parent_id = self.blines[sli.id].parent_id.unwrap();
                let mut parent = &mut self.blines[parent_id];
                parent.nb_kept_children -= 1;
                parent.next_child_idx -= 1; // to fix the number of "unlisted"
                if parent.nb_kept_children == 0 {
                    remove_queue.push(SortableBId {
                        id: parent_id,
                        score: parent.score,
                    });
                }
                count -= 1;
            } else {
                debug!("trimming prematurely interrupted");
                break;
            }
        }
    }

    /// make a tree from the builder's specific structure
    fn take(mut self, out_blines: &[BId]) -> Tree {
        let mut lines: Vec<TreeLine> = Vec::new();
        for id in out_blines.iter() {
            if self.blines[*id].has_match {
                // we need to count the children, so we load them
                if self.blines[*id].file_type.is_dir() && self.blines[*id].children.is_none() {
                    self.load_children(*id);
                }
                if let Ok(tree_line) = self.blines[*id].to_tree_line(self.con) {
                    lines.push(tree_line);
                } else {
                    // I guess the file went missing during tree computation
                    warn!(
                        "Error while builind treeline for {:?}",
                        self.blines[*id].path,
                    );
                }
            }
        }
        let mut tree = Tree {
            lines: lines.into_boxed_slice(),
            selection: 0,
            options: self.options.clone(),
            scroll: 0,
            nb_gitignored: self.nb_gitignored,
            total_search: self.total_search,
            git_status: ComputationResult::None,
        };
        tree.after_lines_changed();
        if let Some(computer) = self.line_status_computer {
            // tree git status is slow to compute, we just mark it should be
            // done (later on)
            tree.git_status = ComputationResult::NotComputed;
            // it would make no sense to keep only files having a git status and
            // not display that type
            for mut line in tree.lines.iter_mut() {
                line.git_status = computer.line_status(&line.path);
            }
        }
        tree
    }

    /// build a tree. Can be called only once per builder.
    ///
    /// Return None if the lifetime expires before end of computation
    /// (usually because the user hit a key)
    pub fn build(mut self, total_search: bool, dam: &Dam) -> Option<Tree> {
        match self.gather_lines(total_search, dam) {
            Some(out_blines) => {
                self.trim_excess(&out_blines);
                Some(self.take(&out_blines))
            }
            None => None, // interrupted
        }
    }
}
