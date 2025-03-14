use {
    crate::{
        app::*,
        command::{Command, TriggerType},
        display::{DisplayableTree, Screen, W},
        errors::{ProgramError, TreeBuildError},
        flag::Flag,
        git,
        pattern::*,
        path::{self, PathAnchor},
        print,
        task_sync::Dam,
        tree::*,
        tree_build::TreeBuilder,
        verb::*,
    },
    open,
    std::path::{Path, PathBuf},
};

/// An application state dedicated to displaying a tree.
/// It's the first and main screen of broot.
pub struct BrowserState {
    pub tree: Tree,
    pub filtered_tree: Option<Tree>,
    pub pending_pattern: InputPattern, // a pattern (or not) which has not yet be applied
    pub total_search_required: bool,   // whether the pending pattern should be in total search mode
    mode: Mode,
}

impl BrowserState {

    /// build a new tree state if there's no error and there's no cancellation.
    ///
    /// In case of cancellation return `Ok(None)`. If dam is `unlimited()` then
    /// this can't be returned.
    pub fn new(
        path: PathBuf,
        mut options: TreeOptions,
        screen: Screen,
        con: &AppContext,
        dam: &Dam,
    ) -> Result<Option<BrowserState>, TreeBuildError> {
        let pending_pattern = options.pattern.take();
        let builder = TreeBuilder::from(
            path,
            options,
            BrowserState::page_height(screen) as usize,
            con,
        )?;
        Ok(builder.build(false, dam).map(move |tree| BrowserState {
            tree,
            filtered_tree: None,
            pending_pattern,
            total_search_required: false,
            mode: initial_mode(con),
        }))
    }

    pub fn root(&self) -> &Path {
        self.tree.root()
    }

    pub fn page_height(screen: Screen) -> i32 {
        i32::from(screen.height) - 2
    }

    /// return a reference to the currently displayed tree, which
    /// is the filtered tree if there's one, the base tree if not.
    pub fn displayed_tree(&self) -> &Tree {
        self.filtered_tree.as_ref().unwrap_or(&self.tree)
    }

    /// return a mutable reference to the currently displayed tree, which
    /// is the filtered tree if there's one, the base tree if not.
    pub fn displayed_tree_mut(&mut self) -> &mut Tree {
        self.filtered_tree.as_mut().unwrap_or(&mut self.tree)
    }

    pub fn open_selection_stay_in_broot(
        &mut self,
        screen: Screen,
        con: &AppContext,
        in_new_panel: bool,
        keep_pattern: bool,
    ) -> Result<CmdResult, ProgramError> {
        let tree = self.displayed_tree();
        let line = tree.selected_line();
        let mut target = line.target().to_path_buf();
        if line.is_dir() {
            if tree.selection == 0 {
                // opening the root would be going to where we already are.
                // We go up one level instead
                if let Some(parent) = target.parent() {
                    target = PathBuf::from(parent);
                }
            }
            let dam = Dam::unlimited();
            Ok(CmdResult::from_optional_state(
                BrowserState::new(
                    target,
                    if keep_pattern {
                        tree.options.clone()
                    } else {
                        tree.options.without_pattern()
                    },
                    screen,
                    con,
                    &dam,
                ),
                in_new_panel,
            ))
        } else {
            match open::that(&target) {
                Ok(exit_status) => {
                    info!("open returned with exit_status {:?}", exit_status);
                    Ok(CmdResult::Keep)
                }
                Err(e) => Ok(CmdResult::error(format!("{:?}", e))),
            }
        }
    }

    pub fn go_to_parent(
        &mut self,
        screen: Screen,
        con: &AppContext,
        in_new_panel: bool,
    ) -> CmdResult {
        match &self.displayed_tree().selected_line().path.parent() {
            Some(path) => CmdResult::from_optional_state(
                BrowserState::new(
                    path.to_path_buf(),
                    self.displayed_tree().options.without_pattern(),
                    screen,
                    con,
                    &Dam::unlimited(),
                ),
                in_new_panel,
            ),
            None => CmdResult::error("no parent found"),
        }
    }

}

impl PanelState for BrowserState {

    fn get_type(&self) -> PanelStateType {
        PanelStateType::Tree
    }

    fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    fn get_mode(&self) -> Mode {
        self.mode
    }

    fn get_pending_task(&self) -> Option<&'static str> {
        if self.pending_pattern.is_some() {
            Some("searching")
        } else if self.displayed_tree().has_dir_missing_sum() {
            Some("computing stats")
        } else if self.displayed_tree().is_missing_git_status_computation() {
            Some("computing git status")
        } else {
            None
        }
    }

    fn selected_path(&self) -> Option<&Path> {
        Some(&self.displayed_tree().selected_line().path)
    }

    fn selection(&self) -> Option<Selection<'_>> {
        Some(self.displayed_tree().selected_line().as_selection())
    }

    fn tree_options(&self) -> TreeOptions {
        self.displayed_tree().options.clone()
    }

    /// build a cmdResult asking for the addition of a new state
    /// being a browser state similar to the current one but with
    /// different options
    fn with_new_options(
        &mut self,
        screen: Screen,
        change_options: &dyn Fn(&mut TreeOptions),
        in_new_panel: bool,
        con: &AppContext,
    ) -> CmdResult {
        let tree = self.displayed_tree();
        let mut options = tree.options.clone();
        change_options(&mut options);
        CmdResult::from_optional_state(
            BrowserState::new(tree.root().clone(), options, screen, con, &Dam::unlimited()),
            in_new_panel,
        )
    }

    fn clear_pending(&mut self) {
        self.pending_pattern = InputPattern::none();
    }

    fn on_click(
        &mut self,
        _x: u16,
        y: u16,
        _screen: Screen,
        _con: &AppContext,
    ) -> Result<CmdResult, ProgramError> {
        self.displayed_tree_mut().try_select_y(y as i32);
        Ok(CmdResult::Keep)
    }

    fn on_double_click(
        &mut self,
        _x: u16,
        y: u16,
        screen: Screen,
        con: &AppContext,
    ) -> Result<CmdResult, ProgramError> {
        if self.displayed_tree().selection == y as usize {
            self.open_selection_stay_in_broot(screen, con, false, false)
        } else {
            // A double click always come after a simple click at
            // same position. If it's not the selected line, it means
            // the click wasn't on a selectable/openable tree line
            Ok(CmdResult::Keep)
        }
    }

    fn on_pattern(
        &mut self,
        pat: InputPattern,
        _app_state: &AppState,
        _con: &AppContext,
    ) -> Result<CmdResult, ProgramError> {
        if pat.is_none() {
            self.filtered_tree = None;
        }
        self.pending_pattern = pat;
        Ok(CmdResult::Keep)
    }

    fn on_internal(
        &mut self,
        w: &mut W,
        internal_exec: &InternalExecution,
        input_invocation: Option<&VerbInvocation>,
        trigger_type: TriggerType,
        app_state: &mut AppState,
        cc: &CmdContext,
    ) -> Result<CmdResult, ProgramError> {
        let con = &cc.app.con;
        let screen = cc.app.screen;
        let page_height = BrowserState::page_height(cc.app.screen);
        let bang = input_invocation
            .map(|inv| inv.bang)
            .unwrap_or(internal_exec.bang);
        Ok(match internal_exec.internal {
            Internal::back => {
                if let Some(filtered_tree) = &self.filtered_tree {
                    let filtered_selection = &filtered_tree.selected_line().path;
                    if self.tree.try_select_path(filtered_selection) {
                        self.tree.make_selection_visible(page_height);
                    }
                    self.filtered_tree = None;
                    CmdResult::Keep
                } else if self.tree.selection > 0 {
                    self.tree.selection = 0;
                    CmdResult::Keep
                } else {
                    CmdResult::PopState
                }
            }
            Internal::focus => internal_focus::on_internal(
                internal_exec,
                input_invocation,
                trigger_type,
                &self.displayed_tree().selected_line().path,
                screen,
                con,
                self.displayed_tree().options.clone(),
            ),
            Internal::up_tree => match self.displayed_tree().root().parent() {
                Some(path) => internal_focus::on_path(
                    path.to_path_buf(),
                    screen,
                    self.displayed_tree().options.clone(),
                    bang,
                    con,
                ),
                None => CmdResult::error("no parent found"),
            },
            Internal::open_stay => self.open_selection_stay_in_broot(screen, con, bang, false)?,
            Internal::open_stay_filter => self.open_selection_stay_in_broot(screen, con, bang, true)?,
            Internal::line_down => {
                let count = get_arg(input_invocation, internal_exec, 1);
                self.displayed_tree_mut().move_selection(count, page_height, true);
                CmdResult::Keep
            }
            Internal::line_up => {
                let count = get_arg(input_invocation, internal_exec, 1);
                self.displayed_tree_mut().move_selection(-count, page_height, true);
                CmdResult::Keep
            }
            Internal::line_down_no_cycle => {
                let count = get_arg(input_invocation, internal_exec, 1);
                self.displayed_tree_mut().move_selection(count, page_height, false);
                CmdResult::Keep
            }
            Internal::line_up_no_cycle => {
                let count = get_arg(input_invocation, internal_exec, 1);
                self.displayed_tree_mut().move_selection(-count, page_height, false);
                CmdResult::Keep
            }
            Internal::previous_match => {
                self.displayed_tree_mut().try_select_previous_match();
                CmdResult::Keep
            }
            Internal::next_match => {
                self.displayed_tree_mut().try_select_next_match();
                CmdResult::Keep
            }
            Internal::previous_same_depth => {
                self.displayed_tree_mut().try_select_previous_same_depth();
                CmdResult::Keep
            }
            Internal::next_same_depth => {
                self.displayed_tree_mut().try_select_next_same_depth();
                CmdResult::Keep
            }
            Internal::page_down => {
                let tree = self.displayed_tree_mut();
                if page_height < tree.lines.len() as i32 {
                    tree.try_scroll(page_height, page_height);
                }
                CmdResult::Keep
            }
            Internal::page_up => {
                let tree = self.displayed_tree_mut();
                if page_height < tree.lines.len() as i32 {
                    tree.try_scroll(-page_height, page_height);
                }
                CmdResult::Keep
            }
            Internal::panel_left => {
                let areas = &cc.panel.areas;
                if areas.is_first() && areas.nb_pos < con.max_panels_count  {
                    // we ask for the creation of a panel to the left
                    internal_focus::new_panel_on_path(
                        self.displayed_tree().selected_line().path.to_path_buf(),
                        screen,
                        self.displayed_tree().options.clone(),
                        PanelPurpose::None,
                        con,
                        HDir::Left,
                    )
                } else {
                    // we let the app handle other cases
                    CmdResult::HandleInApp(Internal::panel_left)
                }
            }
            Internal::panel_right => {
                let areas = &cc.panel.areas;
                let selected_path = &self.displayed_tree().selected_line().path;
                if areas.is_last() && areas.nb_pos < con.max_panels_count {
                    let purpose = if selected_path.is_file() && cc.app.preview_panel.is_none() {
                        PanelPurpose::Preview
                    } else {
                        PanelPurpose::None
                    };
                    // we ask for the creation of a panel to the right
                    internal_focus::new_panel_on_path(
                        selected_path.to_path_buf(),
                        screen,
                        self.displayed_tree().options.clone(),
                        purpose,
                        con,
                        HDir::Right,
                    )
                } else {
                    // we ask the app to handle other cases :
                    // focus the panel to the right or close the leftest one
                    CmdResult::HandleInApp(Internal::panel_right)
                }
            }
            Internal::parent => self.go_to_parent(screen, con, bang),
            Internal::print_path => {
                let path = &self.displayed_tree().selected_line().target();
                print::print_path(path, con)?
            }
            Internal::print_relative_path => {
                let path = &self.displayed_tree().selected_line().target();
                print::print_relative_path(path, con)?
            }
            Internal::print_tree => {
                print::print_tree(&self.displayed_tree(), cc.app.screen, &cc.app.panel_skin, con)?
            }
            Internal::select_first => {
                self.displayed_tree_mut().try_select_first();
                CmdResult::Keep
            }
            Internal::select_last => {
                let page_height = BrowserState::page_height(screen);
                self.displayed_tree_mut().try_select_last(page_height);
                CmdResult::Keep
            }
            Internal::start_end_panel => {
                if cc.panel.purpose.is_arg_edition() {
                    debug!("start_end understood as end");
                    CmdResult::ClosePanel {
                        validate_purpose: true,
                        panel_ref: PanelReference::Active,
                    }
                } else {
                    debug!("start_end understood as start");
                    let tree_options = self.displayed_tree().options.clone();
                    if let Some(input_invocation) = input_invocation {
                        // we'll go for input arg editing
                        let path = if let Some(input_arg) = &input_invocation.args {
                            path::path_from(self.root(), PathAnchor::Unspecified, input_arg)
                        } else {
                            self.root().to_path_buf()
                        };
                        let arg_type = SelectionType::Any; // We might do better later
                        let purpose = PanelPurpose::ArgEdition { arg_type };
                        internal_focus::new_panel_on_path(
                            path, screen, tree_options, purpose, con, HDir::Right,
                        )
                    } else {
                        // we just open a new panel on the selected path,
                        // without purpose
                        internal_focus::new_panel_on_path(
                            self.displayed_tree().selected_line().path.to_path_buf(),
                            screen,
                            tree_options,
                            PanelPurpose::None,
                            con,
                            HDir::Right,
                        )
                    }
                }
            }
            Internal::total_search => {
                if let Some(tree) = &self.filtered_tree {
                    if tree.total_search {
                        CmdResult::error("search was already total - all children have been rated")
                    } else {
                        self.pending_pattern = tree.options.pattern.clone();
                        self.total_search_required = true;
                        CmdResult::Keep
                    }
                } else {
                    CmdResult::error("this verb can be used only after a search")
                }
            }
            Internal::quit => CmdResult::Quit,
            _ => self.on_internal_generic(
                w,
                internal_exec,
                input_invocation,
                trigger_type,
                app_state,
                cc,
            )?,
        })
    }

    fn no_verb_status(
        &self,
        has_previous_state: bool,
        con: &AppContext,
    ) -> Status {
        let mut ssb = con.standard_status.builder(
            PanelStateType::Tree,
            self.displayed_tree().selected_line().as_selection(),
        );
        ssb.has_previous_state = has_previous_state;
        ssb.is_filtered = self.filtered_tree.is_some();
        ssb.has_removed_pattern = false;
        ssb.on_tree_root = self.displayed_tree().selection == 0;
        ssb.status()
    }

    /// do some work, totally or partially, if there's some to do.
    /// Stop as soon as the dam asks for interruption
    fn do_pending_task(
        &mut self,
        screen: Screen,
        con: &AppContext,
        dam: &mut Dam,
    ) {
        if self.pending_pattern.is_some() {
            let pattern_str = self.pending_pattern.raw.clone();
            let mut options = self.tree.options.clone();
            options.pattern = self.pending_pattern.take();
            let root = self.tree.root().clone();
            let page_height = BrowserState::page_height(screen) as usize;
            let builder = match TreeBuilder::from(root, options, page_height, con) {
                Ok(builder) => builder,
                Err(e) => {
                    warn!("Error while preparing tree builder: {:?}", e);
                    return;
                }
            };
            let mut filtered_tree = time!(
                Info,
                "tree filtering",
                &pattern_str,
                builder.build(self.total_search_required, dam),
            ); // can be None if a cancellation was required
            self.total_search_required = false;
            if let Some(ref mut ft) = filtered_tree {
                ft.try_select_best_match();
                ft.make_selection_visible(BrowserState::page_height(screen));
                self.filtered_tree = filtered_tree;
            }
        } else if self.displayed_tree().is_missing_git_status_computation() {
            let root_path = self.displayed_tree().root();
            let git_status = git::get_tree_status(root_path, dam);
            self.displayed_tree_mut().git_status = git_status;
        } else {
            self.displayed_tree_mut().fetch_some_missing_dir_sum(dam, con);
        }
    }

    fn display(
        &mut self,
        w: &mut W,
        disc: &DisplayContext,
    ) -> Result<(), ProgramError> {
        let dp = DisplayableTree {
            app_state: Some(&disc.app_state),
            tree: &self.displayed_tree(),
            skin: &disc.panel_skin.styles,
            ext_colors: &disc.con.ext_colors,
            area: disc.state_area.clone(),
            in_app: true,
        };
        dp.write_on(w)
    }

    fn refresh(&mut self, screen: Screen, con: &AppContext) -> Command {
        let page_height = BrowserState::page_height(screen) as usize;
        // refresh the base tree
        if let Err(e) = self.tree.refresh(page_height, con) {
            warn!("refreshing base tree failed : {:?}", e);
        }
        // refresh the filtered tree, if any
        Command::from_pattern(match self.filtered_tree {
            Some(ref mut tree) => {
                if let Err(e) = tree.refresh(page_height, con) {
                    warn!("refreshing filtered tree failed : {:?}", e);
                }
                &tree.options.pattern
            }
            None => &self.tree.options.pattern,
        })
    }

    fn get_flags(&self) -> Vec<Flag> {
        let options = &self.displayed_tree().options;
        vec![
            Flag {
                name: "h",
                value: if options.show_hidden { "y" } else { "n" },
            },
            Flag {
                name: "gi",
                value: if options.respect_git_ignore { "y" } else { "n" },
            },
        ]
    }

    fn get_starting_input(&self) -> String {
        if self.pending_pattern.is_some() {
            self.pending_pattern.raw.clone()
        } else {
            self.displayed_tree().options.pattern.raw.clone()
        }
    }
}

