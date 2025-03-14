use {
    super::*,
    crate::{
        app::*,
        display::W,
        errors::ProgramError,
        keys,
        skin::PanelSkin,
        verb::{Internal, Verb, VerbExecution},
    },
    crossterm::{
        cursor,
        event::KeyEvent,
        queue,
    },
    termimad::{Area, Event, InputField},
};

/// wrap the input of a panel,
/// receive events and make commands
pub struct PanelInput {
    pub input_field: InputField,
    tab_cycle_count: usize,
    input_before_cycle: Option<String>,
}

impl PanelInput {

    pub fn new(area: Area) -> Self {
        Self {
            input_field: InputField::new(area),
            tab_cycle_count: 0,
            input_before_cycle: None,
        }
    }

    pub fn set_content(&mut self, content: &str) {
        self.input_field.set_content(content);
    }

    pub fn get_content(&self) -> String {
        self.input_field.get_content()
    }

    pub fn display(
        &mut self,
        w: &mut W,
        active: bool,
        mode: Mode,
        mut area: Area,
        panel_skin: &PanelSkin,
    ) -> Result<(), ProgramError> {
        self.input_field.set_normal_style(panel_skin.styles.input.clone());
        self.input_field.focused = active && mode == Mode::Input;
        if mode == Mode::Command && active {
            queue!(w, cursor::MoveTo(area.left, area.top))?;
            panel_skin.styles.mode_command_mark.queue_str(w, "C")?;
            area.width -= 1;
            area.left += 1;
        }
        self.input_field.area = area;
        self.input_field.display_on(w)?;
        Ok(())
    }

    /// consume the event to
    /// - maybe change the input
    /// - build a command
    /// then redraw the input field
    pub fn on_event(
        &mut self,
        w: &mut W,
        event: Event,
        con: &AppContext,
        sel_info: SelInfo<'_>,
        mode: Mode,
    ) -> Result<Command, ProgramError> {
        let cmd = self.get_command(event, con, sel_info, mode);
        self.input_field.display_on(w)?;
        Ok(cmd)
    }

    /// check whether the verb is an action on the input (like
    /// deleting a word) and if it's the case, applies it and
    /// return true
    fn handle_input_related_verb(
        &mut self,
        verb: &Verb,
        _con: &AppContext,
    ) -> bool {
        if let VerbExecution::Internal(internal_exec) = &verb.execution {
            match internal_exec.internal {
                Internal::input_del_char_left => self.input_field.del_char_left(),
                Internal::input_del_char_below => self.input_field.del_char_below(),
                Internal::input_del_word_left => self.input_field.del_word_left(),
                Internal::input_del_word_right => self.input_field.del_word_right(),
                Internal::input_go_left => self.input_field.move_left(),
                Internal::input_go_right => self.input_field.move_right(),
                Internal::input_go_word_left => self.input_field.move_word_left(),
                Internal::input_go_word_right => self.input_field.move_word_right(),
                Internal::input_go_to_start => self.input_field.move_to_start(),
                Internal::input_go_to_end => self.input_field.move_to_end(),
                #[cfg(feature = "clipboard")]
                Internal::input_paste => {
                    match terminal_clipboard::get_string() {
                        Ok(pasted) => {
                            for c in pasted
                                .chars()
                                .filter(|c| c.is_alphanumeric() || c.is_ascii_punctuation())
                            {
                                self.input_field.put_char(c);
                            }
                        }
                        Err(e) => {
                            warn!("Error in reading clipboard: {:?}", e);
                        }
                    }
                    true
                }
                _ => false,
            }
        } else {
            false
        }
    }

    /// when a key is used to enter input mode, we don't always
    /// consume it. Sometimes it should be consumed, sometimes it
    /// should be added to the input
    fn enter_input_mode_with_key(
        &mut self,
        key: KeyEvent,
        parts: &CommandParts,
    ) {
        if let Some(c) = keys::as_letter(key) {
            let add = match c {
                // '/' if !parts.raw_pattern.is_empty() => true,
                ' ' if parts.verb_invocation.is_none() => true,
                ':' if parts.verb_invocation.is_none() => true,
                _ => false,
            };
            if add {
                self.input_field.put_char(c);
            }
        }
    }

    /// consume the event to
    /// - maybe change the input
    /// - build a command
    fn get_command(
        &mut self,
        event: Event,
        con: &AppContext,
        sel_info: SelInfo<'_>,
        mode: Mode,
    ) -> Command {
        match event {
            Event::Click(x, y, ..) => {
                return if self.input_field.apply_event(&event) {
                    Command::empty()
                } else {
                    Command::Click(x, y)
                };
            }
            Event::DoubleClick(x, y) => {
                return Command::DoubleClick(x, y);
            }
            Event::Key(key) => {
                // value of raw and parts before any key related change
                let raw = self.input_field.get_content();
                let parts = CommandParts::from(raw.clone());

                // we first handle the cases that MUST absolutely
                // not be overriden by configuration

                if key == keys::ESC {
                    // tab cycling
                    self.tab_cycle_count = 0;
                    if let Some(raw) = self.input_before_cycle.take() {
                        // we cancel the tab cycling
                        self.input_field.set_content(&raw);
                        self.input_before_cycle = None;
                        return Command::from_raw(raw, false);
                    } else if con.modal && mode == Mode::Input {
                        // leave insertion mode
                        return Command::Internal {
                            internal: Internal::mode_command,
                            input_invocation: None,
                        };
                    } else {
                        // general back command
                        self.input_field.set_content("");
                        let internal = Internal::back;
                        return Command::Internal {
                            internal,
                            input_invocation: parts.verb_invocation,
                        };
                    }
                }

                // tab completion
                if key == keys::TAB {
                    if parts.verb_invocation.is_some() {
                        let parts_before_cycle;
                        let completable_parts = if let Some(s) = &self.input_before_cycle {
                            parts_before_cycle = CommandParts::from(s.clone());
                            &parts_before_cycle
                        } else {
                            &parts
                        };
                        let completions = Completions::for_input(completable_parts, con, sel_info);
                        info!(" -> completions: {:?}", &completions);
                        let added = match completions {
                            Completions::None => {
                                debug!("nothing to complete!");
                                self.tab_cycle_count = 0;
                                self.input_before_cycle = None;
                                None
                            }
                            Completions::Common(completion) => {
                                self.tab_cycle_count = 0;
                                Some(completion)
                            }
                            Completions::List(mut completions) => {
                                let idx = self.tab_cycle_count % completions.len();
                                if self.tab_cycle_count == 0 {
                                    self.input_before_cycle = Some(raw.to_string());
                                }
                                self.tab_cycle_count += 1;
                                Some(completions.swap_remove(idx))
                            }
                        };
                        if let Some(added) = added {
                            let mut raw = self
                                .input_before_cycle
                                .as_ref()
                                .map_or(raw, |s| s.to_string());
                            raw.push_str(&added);
                            self.input_field.set_content(&raw);
                            return Command::from_raw(raw, false);
                        } else {
                            return Command::None;
                        }
                    }
                } else {
                    self.tab_cycle_count = 0;
                    self.input_before_cycle = None;
                }

                if key == keys::ENTER && parts.verb_invocation.is_some() {
                    return Command::from_parts(parts, true);
                }

                if key == keys::QUESTION && (raw.is_empty() || parts.verb_invocation.is_some()) {
                    // a '?' opens the help when it's the first char
                    // or when it's part of the verb invocation
                    return Command::Internal {
                        internal: Internal::help,
                        input_invocation: parts.verb_invocation,
                    };
                }

                // we now check if the key is the trigger key of one of the verbs
                if keys::is_key_allowed_in_mode(key, mode) {
                    for (index, verb) in con.verb_store.verbs.iter().enumerate() {
                        for verb_key in &verb.keys {
                            if *verb_key == key {
                                if self.handle_input_related_verb(verb, con) {
                                    return Command::from_raw(self.input_field.get_content(), false);
                                }
                                if verb.selection_condition.is_respected_by(sel_info.common_stype()) {
                                    if mode != Mode::Input && verb.is_internal(Internal::mode_input) {
                                        self.enter_input_mode_with_key(key, &parts);
                                    }
                                    return Command::VerbTrigger {
                                        index,
                                        input_invocation: parts.verb_invocation,
                                    };
                                } else {
                                    debug!("verb not allowed on current selection");
                                }
                            }
                        }
                    }
                }

                if key == keys::LEFT && raw.is_empty() {
                    let internal = Internal::back;
                    return Command::Internal {
                        internal,
                        input_invocation: parts.verb_invocation,
                    };
                }

                if key == keys::RIGHT && raw.is_empty() {
                    return Command::Internal {
                        internal: Internal::open_stay,
                        input_invocation: None,
                    };
                }

                // input field management
                if mode == Mode::Input {
                    if self.input_field.apply_event(&event) {
                        return Command::from_raw(self.input_field.get_content(), false);
                    }
                }
            }
            Event::Wheel(lines_count) => {
                let internal = if lines_count > 0 {
                    Internal::line_down_no_cycle
                } else {
                    Internal::line_up_no_cycle
                };
                return Command::Internal {
                    internal,
                    input_invocation: None,
                };
            }
            _ => {}
        }
        Command::None
    }
}
