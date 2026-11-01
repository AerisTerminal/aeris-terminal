//! The report dialog opened from the trade history: a period, the accounts, and Generate.
//! Generation runs off the UI thread; its outcome arrives as one notice.

use super::period::{CustomDates, ReportPeriod, ReportZone, date_text, resolve_span};
use super::*;
use bottom_panel::TradeHistoryAccountFilter;

const ACCOUNT_MENU_WIDTH: f32 = 260.0;
const ACCOUNT_MENU_MAX_HEIGHT: f32 = 220.0;
const REPORT_NOTICE_DURATION: Duration = Duration::from_secs(10);
const RUNNING_PROBLEM: &str = "A report is already being generated.";
const NO_ACCOUNT_PROBLEM: &str = "Choose an account to report on.";

/// Choices of the open report dialog. The period fields keep their text while another
/// period is selected.
pub(crate) struct ReportDialog {
    period: ReportPeriod,
    account: TradeHistoryAccountFilter,
    account_menu_open: bool,
    from: Entity<InputState>,
    to: Entity<InputState>,
    problem: Option<&'static str>,
}

impl TerminalApp {
    /// Opens the dialog on the trade history's account filter and the last seven days.
    pub(crate) fn open_trading_report_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let zone = self.trading_report_zone(cx);
        let today = zone.local(self.lifecycle.clock().wall_unix_nanos()).date();
        let week_start = today
            .checked_sub_days(chrono::Days::new(6))
            .unwrap_or(today);
        let from = cx.new(|input_cx| {
            InputState::new(window, input_cx)
                .placeholder("YYYY-MM-DD")
                .default_value(date_text(week_start))
        });
        let to = cx.new(|input_cx| {
            InputState::new(window, input_cx)
                .placeholder("YYYY-MM-DD")
                .default_value(date_text(today))
        });
        let panel = &mut self.pages.bottom_panel;
        panel.account_menu_open = false;
        panel.report = Some(ReportDialog {
            period: ReportPeriod::default(),
            account: panel.account_filter().clone(),
            account_menu_open: false,
            from,
            to,
            problem: None,
        });
        cx.notify();
    }

    fn close_trading_report_dialog(&mut self, cx: &mut Context<Self>) {
        if self.pages.bottom_panel.report.take().is_some() {
            cx.notify();
        }
    }

    fn update_trading_report_dialog(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut ReportDialog),
    ) {
        if let Some(dialog) = self.pages.bottom_panel.report.as_mut() {
            change(dialog);
            cx.notify();
        }
    }

    /// The active chart's display zone, so the report reads like the trade history list.
    fn trading_report_zone(&self, cx: &App) -> ReportZone {
        ReportZone::new(
            self.active_surface()
                .read(cx)
                .chart
                .as_ref()
                .map_or(aeris_chart_integration::DEFAULT_TIME_ZONE, |chart| {
                    chart.read(cx).time_zone_id()
                }),
        )
    }

    fn generate_trading_report(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.pages.bottom_panel.report.as_ref() else {
            return;
        };
        let period = dialog.period;
        let account = dialog.account.clone();
        let from = dialog.from.read(cx).value().to_string();
        let to = dialog.to.read(cx).value().to_string();
        if self.pages.bottom_panel.report_running {
            self.update_trading_report_dialog(cx, |dialog| dialog.problem = Some(RUNNING_PROBLEM));
            return;
        }
        let zone = self.trading_report_zone(cx);
        let now = self.lifecycle.clock().wall_unix_nanos();
        let span = match resolve_span(
            period,
            &CustomDates {
                from: &from,
                to: &to,
            },
            now,
            zone,
        ) {
            Ok(span) => span,
            Err(problem) => {
                self.update_trading_report_dialog(cx, |dialog| {
                    dialog.problem = Some(problem.message());
                });
                return;
            }
        };
        let accounts = self
            .pages
            .bottom_panel
            .report_accounts()
            .iter()
            .filter(|candidate| account.includes(&candidate.id))
            .cloned()
            .collect::<Vec<_>>();
        if accounts.is_empty() {
            self.update_trading_report_dialog(cx, |dialog| {
                dialog.problem = Some(NO_ACCOUNT_PROBLEM);
            });
            return;
        }
        let scope = match account {
            TradeHistoryAccountFilter::All => "All accounts".to_string(),
            TradeHistoryAccountFilter::Account(_) => accounts
                .first()
                .map(|account| account.display_name.clone())
                .unwrap_or_default(),
        };
        let request = ReportRequest {
            accounts,
            symbols: self.pages.bottom_panel.report_symbols(),
            zone,
            span,
            period,
            scope,
            generated_unix_nanos: now,
            directory: self.lifecycle.persistence().reports_directory(),
            theme: self.theme,
        };
        self.pages.bottom_panel.report = None;
        self.pages.bottom_panel.report_running = true;
        let service = self.lifecycle.trading().clone();
        cx.spawn(async move |terminal, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { generate(&service, &request) })
                .await;
            // A retired terminal has no notice center left to show the outcome.
            terminal
                .update(cx, |terminal, terminal_cx| {
                    terminal.pages.bottom_panel.report_running = false;
                    terminal.post_trading_report_notice(result, terminal_cx);
                })
                .unwrap_or(());
        })
        .detach();
        cx.notify();
    }

    fn post_trading_report_notice(
        &mut self,
        result: Result<GeneratedReport, ReportError>,
        cx: &mut Context<Self>,
    ) {
        let trades = |count: usize| {
            if count == 1 {
                "1 closed trade".to_string()
            } else {
                format!("{count} closed trades")
            }
        };
        let (severity, tone, title, detail, action) = match result {
            Ok(report) if report.opened => (
                notification_center::NoticeSeverity::Info,
                notification_center::NoticeTone::Info,
                "Trading report ready",
                format!("{}, opened in your browser.", trades(report.trades)),
                Some(notification_center::NoticeAction::OpenFolder(report.path)),
            ),
            Ok(report) => (
                notification_center::NoticeSeverity::Warning,
                notification_center::NoticeTone::Warning,
                "Trading report saved",
                format!("{}. Your browser could not open it.", trades(report.trades)),
                Some(notification_center::NoticeAction::OpenFolder(report.path)),
            ),
            Err(error) => {
                match &error {
                    ReportError::History(detail) => {
                        diagnostic!("Aeris trading report could not read history: {detail}");
                    }
                    ReportError::AmountOverflow => {
                        diagnostic!("Aeris trading report total overflowed");
                    }
                    ReportError::Render => {
                        diagnostic!("Aeris trading report could not be composed");
                    }
                    ReportError::Write(detail) => {
                        diagnostic!("Aeris trading report could not be saved: {detail}");
                    }
                }
                (
                    notification_center::NoticeSeverity::Warning,
                    notification_center::NoticeTone::Warning,
                    "Trading report failed",
                    error.notice_detail().to_string(),
                    None,
                )
            }
        };
        let Some(host) = toast_host(self.tabs.active()) else {
            return;
        };
        self.notices.toaster_mut().post_notice(
            host,
            notification_center::UserNotice {
                key: notification_center::NoticeKey {
                    scope: 0,
                    kind: "trading_report",
                },
                severity,
                tone,
                source: notification_center::NoticeSource::Workspace,
                title,
                detail,
                action,
                lifetime: notification_center::NoticeLifetime::Timed(REPORT_NOTICE_DURATION),
            },
            Instant::now(),
        );
        cx.notify();
    }

    pub(crate) fn trading_report_layer(
        &self,
        terminal: &Entity<Self>,
        cx: &App,
    ) -> Option<AnyElement> {
        let dialog = self.pages.bottom_panel.report.as_ref()?;
        let theme = self.theme;
        let colors = theme.colors;
        let accounts = self.pages.bottom_panel.report_accounts();
        let dismiss = terminal.clone();
        let cancel = terminal.clone();
        let generate = terminal.clone();
        let zone = self.trading_report_zone(cx).id();
        Some(
            Dialog::new(
                "trading_report_dialog",
                DialogSize::Md,
                &theme,
                move |_, cx| {
                    dismiss.update(cx, TerminalApp::close_trading_report_dialog);
                },
            )
            .title("Trading report")
            .subtitle("Closed trades as an offline HTML file")
            .child(report_field(
                "Period",
                report_period_choices(terminal, dialog.period, &theme),
                &theme,
            ))
            .when(dialog.period == ReportPeriod::Custom, |body| {
                body.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            report_field(
                                "First day",
                                Input::new(&dialog.from).platform(&theme),
                                &theme,
                            )
                            .flex_1(),
                        )
                        .child(
                            report_field(
                                "Last day",
                                Input::new(&dialog.to).platform(&theme),
                                &theme,
                            )
                            .flex_1(),
                        ),
                )
            })
            .child(report_field(
                "Accounts",
                report_account_picker(terminal, dialog, accounts, &theme),
                &theme,
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(gpui_color(colors.text_secondary))
                    .child(format!(
                        "Days and hours use the chart's time zone ({zone}). Simulated and broker results are reported separately. The report is saved in Downloads, in the Aeris Reports folder, and opens in your browser."
                    )),
            )
            .children(dialog.problem.map(|problem| {
                div()
                    .text_xs()
                    .text_color(gpui_color(colors.text_danger))
                    .child(problem)
            }))
            .action(
                Button::new("trading_report_cancel", &theme)
                    .variant(ButtonVariant::Secondary)
                    .label("Cancel")
                    .on_press(move |_, _, cx| {
                        cancel.update(cx, TerminalApp::close_trading_report_dialog);
                    }),
            )
            .action(
                Button::new("trading_report_generate", &theme)
                    .variant(ButtonVariant::Primary)
                    .label("Generate report")
                    .disabled(self.pages.bottom_panel.report_running)
                    .on_press(move |_, _, cx| {
                        generate.update(cx, TerminalApp::generate_trading_report);
                    }),
            )
            .into_any_element(),
        )
    }
}

fn report_field(label: &'static str, control: impl IntoElement, theme: &AerisTheme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_secondary))
                .child(label),
        )
        .child(control)
}

fn report_period_choices(
    terminal: &Entity<TerminalApp>,
    selected: ReportPeriod,
    theme: &AerisTheme,
) -> Div {
    div()
        .flex()
        .flex_wrap()
        .gap_1()
        .children(
            ReportPeriod::ALL
                .into_iter()
                .enumerate()
                .map(|(index, period)| {
                    let select = terminal.clone();
                    Button::new(("trading_report_period", index), theme)
                        .variant(ButtonVariant::Outline)
                        .button_size(ButtonSize::Sm)
                        .selected(selected == period)
                        .label(period.label())
                        .on_press(move |_, _, cx| {
                            select.update(cx, |terminal, terminal_cx| {
                                terminal.update_trading_report_dialog(terminal_cx, |dialog| {
                                    dialog.period = period;
                                    dialog.problem = None;
                                });
                            });
                        })
                }),
        )
}

fn report_account_picker(
    terminal: &Entity<TerminalApp>,
    dialog: &ReportDialog,
    accounts: &[aeris_trading::TradingAccount],
    theme: &AerisTheme,
) -> impl IntoElement {
    let toggle = terminal.clone();
    let dismiss = terminal.clone();
    let label = match &dialog.account {
        TradeHistoryAccountFilter::All => "All accounts".to_string(),
        TradeHistoryAccountFilter::Account(key) => accounts
            .iter()
            .find(|account| account.id.as_str() == key)
            .map_or_else(|| key.clone(), |account| account.display_name.clone()),
    };
    let trigger = Button::new("trading_report_account", theme)
        .variant(ButtonVariant::Outline)
        .button_size(ButtonSize::Sm)
        .open(dialog.account_menu_open)
        .aria_label("Accounts in the report")
        .label(label)
        .caret(header_icon(HugeIcon::ChevronDown))
        .on_press(move |_, _, cx| {
            toggle.update(cx, |terminal, terminal_cx| {
                terminal.update_trading_report_dialog(terminal_cx, |dialog| {
                    dialog.account_menu_open = !dialog.account_menu_open;
                });
            });
        });
    let menu = dialog.account_menu_open.then(|| {
        let choices = std::iter::once((TradeHistoryAccountFilter::All, "All accounts".to_string()))
            .chain(accounts.iter().map(|account| {
                (
                    TradeHistoryAccountFilter::Account(account.id.as_str().to_string()),
                    account.display_name.clone(),
                )
            }));
        let mut menu = MenuPanel::new(
            "trading_report_account_menu",
            MenuPlacement::Anchored {
                side: MenuSide::Below,
                align: MenuAlign::Start,
            },
            theme,
        )
        .width(px(ACCOUNT_MENU_WIDTH))
        .max_height(px(ACCOUNT_MENU_MAX_HEIGHT));
        for (index, (choice, name)) in choices.enumerate() {
            let select = terminal.clone();
            let checked = dialog.account == choice;
            menu = menu.child(
                MenuRow::compact(("trading_report_account_choice", index), name, theme)
                    .checked(checked)
                    .on_click(move |_, _, cx| {
                        let choice = choice.clone();
                        select.update(cx, |terminal, terminal_cx| {
                            terminal.update_trading_report_dialog(terminal_cx, |dialog| {
                                dialog.account = choice;
                                dialog.account_menu_open = false;
                                dialog.problem = None;
                            });
                        });
                    }),
            );
        }
        menu
    });
    MenuAnchor::new("trading_report_account_anchor", trigger)
        .menu(menu)
        .on_dismiss(move |_, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.update_trading_report_dialog(terminal_cx, |dialog| {
                    dialog.account_menu_open = false;
                });
            });
        })
}
