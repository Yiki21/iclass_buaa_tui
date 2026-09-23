use super::*;

#[derive(Clone, Debug)]

pub struct VenueForm {
    pub fields:   [String; 4],
    pub selected: usize,
    pub error:    Option<String>,
}

impl App {
    pub(super) fn open_venue_form(&mut self) {

        self.venue.form = Some(VenueForm {
            fields:   [
                self.venue.phone.clone(),
                self.venue.theme.clone(),
                if self.venue.day_date.is_empty() {

                    Local::now().date_naive().to_string()
                } else {

                    self.venue.day_date.clone()
                },
                self.venue.joiners.max(1).to_string(),
            ],
            selected: 0,
            error:    None,
        });
    }

    pub(super) fn handle_venue_form_key(
        &mut self,
        key: KeyEvent,
        tx: &UnboundedSender<AsyncEvent>,
    ) {

        let Some(form) = self.venue.form.as_mut() else {

            return;
        };

        match key.code {
            KeyCode::Esc => {

                self.venue.form = None;
            }
            KeyCode::Tab | KeyCode::Down => form.selected = (form.selected + 1) % 4,
            KeyCode::BackTab | KeyCode::Up => form.selected = (form.selected + 3) % 4,
            KeyCode::Backspace => {

                form.fields[form.selected].pop();

                form.error = None;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                form.fields[form.selected].clear()
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {

                if form.fields[form.selected].chars().count() < 120 {

                    form.fields[form.selected].push(ch);
                }

                form.error = None;
            }
            KeyCode::Enter => {

                let phone = form.fields[0].trim().to_owned();

                let theme = form.fields[1].trim().to_owned();

                let date = form.fields[2].trim().to_owned();

                let count = form.fields[3].trim().parse::<i64>().ok().filter(|n| *n > 0);

                if phone.is_empty() || theme.is_empty() {

                    form.error = Some("联系电话和主题不能为空".into());

                    return;
                }

                if NaiveDate::parse_from_str(&date, "%Y-%m-%d").is_err() || count.is_none() {

                    form.error = Some("日期格式为 YYYY-MM-DD，人数必须为正整数".into());

                    return;
                }

                let changed_date = self.venue.day_date != date;

                self.venue.phone = phone;

                self.venue.theme = theme;

                self.venue.day_date = date;

                self.venue.joiners = count.unwrap_or(1);

                self.venue.form = None;

                if changed_date {

                    self.venue.day = None;

                    self.venue.day_loading = false;

                    self.venue.chosen_slots.clear();

                    if !self.venue.sites.is_empty() {

                        self.load_venue_day(tx);
                    }
                }

                self.info("预约信息已保存；尚未提交预约");
            }
            _ => {}
        }
    }

    pub(super) fn move_venue_space(&mut self, delta: isize) {

        let Some(day) = self.venue.day.as_ref() else {

            return;
        };

        self.venue.selected_space = clamp_step(self.venue.selected_space, day.spaces.len(), delta);

        self.venue.chosen_slots.clear();

        self.venue.selected_slot = 0;
    }
}
