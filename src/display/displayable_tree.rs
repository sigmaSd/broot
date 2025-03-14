use {
    super::{
        Col,
        CropWriter,
        GitStatusDisplay,
        SPACE_FILLING, BRANCH_FILLING,
        MatchedString,
    },
    crate::{
        app::AppState,
        content_search::ContentMatch,
        errors::ProgramError,
        file_sum::FileSum,
        pattern::PatternObject,
        skin::{ExtColorMap, StyleMap},
        task_sync::ComputationResult,
        tree::{Tree, TreeLine, TreeLineType},
    },
    chrono::{DateTime, Local, TimeZone},
    crossterm::{
        cursor,
        QueueableCommand,
    },
    file_size,
    git2::Status,
    std::io::Write,
    termimad::{CompoundStyle, ProgressBar},
};

/// A tree wrapper which can be used either
/// - to write on the screen in the application,
/// - or to write in a file or an exported string.
/// Using it in the application (with in_app true) means that
///  - the selection is drawn
///  - a scrollbar may be drawn
///  - the empty lines will be erased
pub struct DisplayableTree<'a, 's, 't> {
    pub app_state: Option<&'a AppState>,
    pub tree: &'t Tree,
    pub skin: &'s StyleMap,
    pub area: termimad::Area,
    pub in_app: bool, // if true we show the selection and scrollbar
    pub ext_colors: &'s ExtColorMap,
}

impl<'a, 's, 't> DisplayableTree<'a, 's, 't> {

    pub fn out_of_app(
        tree: &'t Tree,
        skin: &'s StyleMap,
        ext_colors: &'s ExtColorMap,
        width: u16,
        height: u16,
    ) -> DisplayableTree<'a, 's, 't> {
        DisplayableTree {
            app_state: None,
            tree,
            skin,
            ext_colors,
            area: termimad::Area {
                left: 0,
                top: 0,
                width,
                height,
            },
            in_app: false,
        }
    }

    fn label_style(
        &self,
        line: &TreeLine,
        selected: bool,
    ) -> CompoundStyle {
        let style = match &line.line_type {
            TreeLineType::Dir => &self.skin.directory,
            TreeLineType::File => {
                if line.is_exe() {
                    &self.skin.exe
                } else {
                    &self.skin.file
                }
            }
            TreeLineType::BrokenSymLink(_) | TreeLineType::SymLink { .. } => &self.skin.link,
            TreeLineType::Pruning => &self.skin.pruning,
        };
        let mut style = style.clone();
        if let Some(ext_color) = line.extension().and_then(|ext| self.ext_colors.get(ext)) {
            style.set_fg(ext_color);
        }
        if selected {
            if let Some(c) = self.skin.selected_line.get_bg() {
                style.set_bg(c);
            }
        }
        style
    }

    fn write_line_count<'w, W: Write>(
        &self,
        cw: &mut CropWriter<'w, W>,
        line: &TreeLine,
        count_len: usize,
        selected: bool,
    ) -> Result<usize, termimad::Error> {
        Ok(if let Some(s) = line.sum {
            cond_bg!(count_style, self, selected, self.skin.count);
            cw.queue_g_string(&count_style, format!("{:>width$}", s.to_count(), width=count_len))?;
            1
        } else {
            count_len + 1
        })
    }

    fn write_line_selection_mark<'w, W: Write>(
        &self,
        cw: &mut CropWriter<'w, W>,
        style: &CompoundStyle,
        selected: bool,
    ) -> Result<usize, termimad::Error> {
        Ok(if selected {
            cw.queue_char(&style, '▶')?;
            0
        } else {
            1
        })
    }

    fn write_line_size<'w, W: Write>(
        &self,
        cw: &mut CropWriter<'w, W>,
        line: &TreeLine,
        style: &CompoundStyle,
        _selected: bool,
    ) -> Result<usize, termimad::Error> {
        Ok(if let Some(s) = line.sum {
            cw.queue_g_string(
                style,
                format!("{:>4}", file_size::fit_4(s.to_size())),
            )?;
            1
        } else {
            5
        })
    }

    /// only makes sense when there's only one level
    /// (so in sort mode)
    fn write_line_size_with_bar<'w, W: Write>(
        &self,
        cw: &mut CropWriter<'w, W>,
        line: &TreeLine,
        label_style: &CompoundStyle,
        total_size: FileSum,
        selected: bool,
    ) -> Result<usize, termimad::Error> {
        Ok(if let Some(s) = line.sum {
            let pb = ProgressBar::new(s.part_of_size(total_size), 10);
            cond_bg!(sparse_style, self, selected, self.skin.sparse);
            cw.queue_g_string(
                label_style,
                format!("{:>4}", file_size::fit_4(s.to_size())),
            )?;
            cw.queue_char(
                &sparse_style,
                if s.is_sparse() && line.is_file() { 's' } else { ' ' },
            )?;
            cw.queue_g_string(label_style, format!("{:<10}", pb))?;
            1
        } else {
            16
        })
    }

    fn write_line_git_status<'w, W: Write>(
        &self,
        cw: &mut CropWriter<'w, W>,
        line: &TreeLine,
        selected: bool,
    ) -> Result<usize, termimad::Error> {
        let (style, char) = if !line.is_selectable() {
            (&self.skin.tree, ' ')
        } else {
            match line.git_status.map(|s| s.status) {
                Some(Status::CURRENT) => (&self.skin.git_status_current, ' '),
                Some(Status::WT_NEW) => (&self.skin.git_status_new, 'N'),
                Some(Status::CONFLICTED) => (&self.skin.git_status_conflicted, 'C'),
                Some(Status::WT_MODIFIED) => (&self.skin.git_status_modified, 'M'),
                Some(Status::IGNORED) => (&self.skin.git_status_ignored, 'I'),
                None => (&self.skin.tree, ' '),
                _ => (&self.skin.git_status_other, '?'),
            }
        };
        cond_bg!(git_style, self, selected, style);
        cw.queue_char(git_style, char)?;
        Ok(0)
    }

    fn write_date<'w, W: Write>(
        &self,
        cw: &mut CropWriter<'w, W>,
        seconds: i64,
        selected: bool,
    ) -> Result<usize, termimad::Error> {
        let date_time: DateTime<Local> = Local.timestamp(seconds, 0);
        cond_bg!(date_style, self, selected, self.skin.dates);
        cw.queue_g_string(
            date_style,
            date_time
                .format(self.tree.options.date_time_format)
                .to_string(),
        )?;
        Ok(1)
    }

    fn write_branch<'w, W: Write>(
        &self,
        cw: &mut CropWriter<'w, W>,
        line_index: usize,
        line: &TreeLine,
        selected: bool,
        staged: bool,
    ) -> Result<usize, ProgramError> {
        cond_bg!(branch_style, self, selected, self.skin.tree);
        let mut branch = String::new();
        for depth in 0..line.depth {
            branch.push_str(
                if line.left_branchs[depth as usize] {
                    if self.tree.has_branch(line_index + 1, depth as usize) {
                        // TODO: If a theme is on, remove the horizontal lines
                        if depth == line.depth - 1 {
                            if staged {
                                "├◍─"
                            } else {
                                "├──"
                            }
                        } else {
                            "│  "
                        }
                    } else {
                        if staged {
                            "└◍─"
                        } else {
                            "└──"
                        }
                    }
                } else {
                    "   "
                },
            );
        }
        if !branch.is_empty() {
            cw.queue_g_string(&branch_style, branch)?;
        }
        Ok(0)
    }

    /// write the symbol showing whether the path is staged
    fn write_line_stage_mark<'w, W: Write>(
        &self,
        cw: &mut CropWriter<'w, W>,
        style: &CompoundStyle,
        staged: bool,
    ) -> Result<usize, termimad::Error> {
        Ok(if staged {
            cw.queue_char(&style, '◍')?; // ▣
            0
        } else {
            1
        })
    }

    /// write the name or subpath, depending on the pattern_object
    fn write_line_label<'w, W: Write>(
        &self,
        cw: &mut CropWriter<'w, W>,
        line: &TreeLine,
        style: &CompoundStyle,
        pattern_object: PatternObject,
        selected: bool,
    ) -> Result<usize, ProgramError> {
        cond_bg!(char_match_style, self, selected, self.skin.char_match);
        if let Some(icon) = line.icon {
            cw.queue_char(style, icon)?;
            cw.queue_char(style, ' ')?;
            cw.queue_char(style, ' ')?;
        }
        let label = if pattern_object.subpath {
            &line.subpath
        } else {
            &line.name
        };
        let name_match = self.tree.options.pattern.pattern.search_string(label);
        let matched_string = MatchedString::new(
            name_match,
            label,
            &style,
            &char_match_style,
        );
        matched_string.queue_on(cw)?;
        match &line.line_type {
            TreeLineType::Dir => {
                if line.unlisted > 0 {
                    cw.queue_str(style, " …")?;
                }
            }
            TreeLineType::BrokenSymLink(direct_path) => {
                cw.queue_str(style, " -> ")?;
                cond_bg!(error_style, self, selected, self.skin.file_error);
                cw.queue_str(error_style, &direct_path)?;
            }
            TreeLineType::SymLink {
                final_is_dir,
                direct_target,
                ..
            } => {
                cw.queue_str(style, " -> ")?;
                let target_style = if *final_is_dir {
                    &self.skin.directory
                } else {
                    &self.skin.file
                };
                cond_bg!(target_style, self, selected, target_style);
                cw.queue_str(target_style, &direct_target)?;
            }
            _ => {}
        }
        Ok(1)
    }

    fn write_content_extract<'w, W: Write>(
        &self,
        cw: &mut CropWriter<'w, W>,
        extract: ContentMatch,
        selected: bool,
    ) -> Result<(), ProgramError> {
        cond_bg!(extract_style, self, selected, self.skin.content_extract);
        cond_bg!(match_style, self, selected, self.skin.content_match);
        cw.queue_str(&extract_style, "  ")?;
        if extract.needle_start > 0 {
            cw.queue_str(&extract_style, &extract.extract[0..extract.needle_start])?;
        }
        cw.queue_str(
            &match_style,
            &extract.extract[extract.needle_start..extract.needle_end],
        )?;
        if extract.needle_end < extract.extract.len() {
            cw.queue_str(&extract_style, &extract.extract[extract.needle_end..])?;
        }
        Ok(())
    }

    pub fn write_root_line<'w, W: Write>(
        &self,
        cw: &mut CropWriter<'w, W>,
        selected: bool,
    ) -> Result<(), ProgramError> {
        cond_bg!(style, self, selected, self.skin.directory);
        let line = &self.tree.lines[0];
        if self.tree.options.show_sizes {
            if let Some(s) = line.sum {
                cw.queue_g_string(
                    style,
                    format!("{:>4} ", file_size::fit_4(s.to_size())),
                )?;
            }
        }
        let title = line.path.to_string_lossy();
        cw.queue_str(&style, &title)?;
        if self.in_app && !cw.is_full() {
            if let ComputationResult::Done(git_status) = &self.tree.git_status {
                let git_status_display = GitStatusDisplay::from(
                    git_status,
                    &self.skin,
                    cw.allowed,
                );
                git_status_display.write(cw, selected)?;
            }
            #[cfg(unix)]
            if self.tree.options.show_root_fs {
                if let Some(mount) = line.mount() {
                    let fs_space_display = crate::filesystems::MountSpaceDisplay::from(
                        &mount,
                        &self.skin,
                        cw.allowed,
                    );
                    fs_space_display.write(cw, selected)?;
                }
            }
            self.extend_line_bg(cw, selected)?;
        }
        Ok(())
    }

    /// if in app, extend the background till the end of screen row
    pub fn extend_line_bg<'w, W: Write>(
        &self,
        cw: &mut CropWriter<'w, W>,
        selected: bool,
    ) -> Result<(), ProgramError> {
        if self.in_app && !cw.is_full() {
            let style = if selected {
                &self.skin.selected_line
            } else {
                &self.skin.default
            };
            cw.fill(style, &SPACE_FILLING)?;
        }
        Ok(())
    }

    /// write the whole tree on the given `W`
    pub fn write_on<W: Write>(&self, f: &mut W) -> Result<(), ProgramError> {
        #[cfg(not(any(target_family = "windows", target_os = "android")))]
        let perm_writer = super::PermWriter::for_tree(&self.skin, &self.tree);

        let tree = self.tree;
        let total_size = tree.total_sum();
        let scrollbar = if self.in_app {
            self.area.scrollbar(tree.scroll, tree.lines.len() as i32 - 1)
        } else {
            None
        };
        if self.in_app {
            f.queue(cursor::MoveTo(self.area.left, self.area.top))?;
        }
        let mut cw = CropWriter::new(f, self.area.width as usize);
        let pattern_object = tree.options.pattern.pattern.object();
        self.write_root_line(&mut cw, self.in_app && tree.selection == 0)?;
        self.skin.queue_reset(f)?;

        let visible_cols: Vec<Col> = tree
            .options
            .cols_order
            .iter()
            .filter(|col| col.is_visible(&tree, self.app_state))
            .cloned()
            .collect();

        // if necessary we compute the width of the count column
        let count_len = if tree.options.show_counts {
            tree.lines.iter()
                .skip(1) // we don't show the counts of the root
                .map(|l| l.sum.map_or(0, |s| s.to_count()))
                .max()
                .map(|c| format!("{}", c).len())
                .unwrap_or(0)
        } else {
            0
        };

        // we compute the length of the dates, depending on the format
        let date_len = if tree.options.show_dates {
            let date_time: DateTime<Local> = Local::now();
            date_time.format(tree.options.date_time_format).to_string().len()
        } else {
            0 // we don't care
        };

        for y in 1..self.area.height {
            if self.in_app {
                f.queue(cursor::MoveTo(self.area.left, y + self.area.top))?;
            } else {
                write!(f, "\r\n")?;
            }
            let mut line_index = y as usize;
            if line_index > 0 {
                line_index += tree.scroll as usize;
            }
            let mut selected = false;
            let mut cw = CropWriter::new(f, self.area.width as usize);
            let cw = &mut cw;
            if line_index < tree.lines.len() {
                let line = &tree.lines[line_index];
                selected = self.in_app && line_index == tree.selection;
                let label_style = self.label_style(line, selected);
                let mut in_branch = false;
                let space_style = if selected {
                    &self.skin.selected_line
                } else {
                    &self.skin.default
                };
                if visible_cols[0].needs_left_margin() {
                    cw.queue_char(space_style, ' ')?;
                }
                let staged = self.app_state
                    .map_or(false, |a| a.stage.contains(&line.path));
                for col in &visible_cols {
                    let void_len = match col {

                        Col::Mark => {
                            self.write_line_selection_mark(cw, &label_style, selected)?
                        }

                        Col::Git => {
                            self.write_line_git_status(cw, line, selected)?
                        }

                        Col::Branch => {
                            in_branch = true;
                            self.write_branch(cw, line_index, line, selected, staged)?
                        }

                        Col::Permission => {
                            #[cfg(any(target_family = "windows", target_os = "android"))]
                            { 0 }

                            #[cfg(not(any(target_family = "windows", target_os = "android")))]
                            perm_writer.write_permissions(cw, line, selected)?
                        }

                        Col::Date => {
                            if let Some(seconds) = line.sum.and_then(|sum| sum.to_valid_seconds()) {
                                self.write_date(cw, seconds, selected)?
                            } else {
                                date_len + 1
                            }
                        }

                        Col::Size => {
                            if tree.options.sort.is_some() {
                                // as soon as there's only one level displayed we can show the size bars
                                self.write_line_size_with_bar(cw, line, &label_style, total_size, selected)?
                            } else {
                                self.write_line_size(cw, line, &label_style, selected)?
                            }
                        }

                        Col::Count => {
                            self.write_line_count(cw, line, count_len, selected)?
                        }

                        Col::Staged => {
                            self.write_line_stage_mark(cw, &label_style, staged)?
                        }

                        Col::Name => {
                            in_branch = false;
                            self.write_line_label(cw, line, &label_style, pattern_object, selected)?
                        }

                    };
                    // void: intercol & replacing missing cells
                    if in_branch && void_len > 2 {
                        cond_bg!(void_style, self, selected, &self.skin.tree);
                        cw.repeat(void_style, &BRANCH_FILLING, void_len)?;
                    } else {
                        cond_bg!(void_style, self, selected, &self.skin.default);
                        cw.repeat(void_style, &SPACE_FILLING, void_len)?;
                    }
                }

                if cw.allowed > 8 && pattern_object.content {
                    let extract = tree.options.pattern.pattern.search_content(&line.path, cw.allowed - 2);
                    if let Some(extract) = extract {
                        self.write_content_extract(cw, extract, selected)?;
                    }
                }
            }
            self.extend_line_bg(cw, selected)?;
            self.skin.queue_reset(f)?;
            if self.in_app && y > 0 {
                if let Some((sctop, scbottom)) = scrollbar {
                    f.queue(cursor::MoveTo(self.area.left + self.area.width - 1, y))?;
                    let style = if sctop <= y && y <= scbottom {
                        &self.skin.scrollbar_thumb
                    } else {
                        &self.skin.scrollbar_track
                    };
                    style.queue_str(f, "▐")?;
                }
            }
        }
        if !self.in_app {
            write!(f, "\r\n")?;
        }
        Ok(())
    }
}

