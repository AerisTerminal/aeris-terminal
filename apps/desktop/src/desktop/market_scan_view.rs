//! The Scans tab of the screener page: the saved scan library, the builder for the scan being
//! edited and the markets the running scan matches. Selecting a match opens it in the terminal.

use super::chrome_menu::chrome_menu_group_heading;
use super::market_scan::{
    COMPARISONS, ConditionDraft, MATCH_MODES, MarketScan, OperandDraft, OperandKind, OperandPart,
    SCAN_UNIVERSE_CHOICES, ScanDraft, ScanField, ScanMenu, ScanResultColumn, ScanResultRow,
    ScanRun, ScanRunNotice, ScanSide, comparison_label, family_label, line_label, match_mode_label,
    operand_groups, parameter_label, run_summary,
};
use super::market_screener::{MarketScreener, ScreenerTab};
use super::market_screener_view::{
    CONTROL_HEIGHT, EXCHANGE_LOGO_SIZE, FOOTER_HEIGHT, MARKET_MIN_WIDTH, ROW_HEIGHT,
    SORT_ICON_SIZE, TABLE_HEADER_HEIGHT, direction_color, rank_cell, row_menu_on_right_click,
    table_cell, table_inner_radius, table_row_frame, watchlist_status_line,
};
use super::saved_scans::MAXIMUM_SAVED_SCANS;
use super::*;
use aeris_localization::{panels, tr};
use aeris_market_runtime::{ScanComparison, ScanStudyParameterKind};
use gpui::{ClickEvent, uniform_list};

const SECTION_GAP: f32 = 16.0;
const BUILDER_GAP: f32 = 8.0;
const LIBRARY_WIDTH: f32 = 220.0;
const NAME_WIDTH: f32 = 220.0;
const PART_WIDTH: f32 = 56.0;
const NUMBER_WIDTH: f32 = 96.0;
const CONDITION_NUMBER_WIDTH: f32 = 20.0;
const MENU_WIDTH: f32 = 240.0;
const MENU_MAX_HEIGHT: f32 = 360.0;
/// The operand menu lists every study with its full name, so it is wider and taller.
const OPERAND_MENU_WIDTH: f32 = 340.0;
const OPERAND_MENU_MAX_HEIGHT: f32 = 440.0;
const CLOSE_WIDTH: f32 = 128.0;
const CHANGE_WIDTH: f32 = 96.0;
const VOLUME_WIDTH: f32 = 112.0;
const VALUE_COLUMN_WIDTH: f32 = 128.0;
/// Market, close, change and volume lead every result row; condition values follow.
const LEADING_RESULT_COLUMNS: usize = 4;

/// What every part of the tab reads.
#[derive(Clone, Copy)]
struct Ui<'a> {
    terminal: &'a Entity<TerminalApp>,
    scans: &'a MarketScan,
    theme: &'a AerisTheme,
}

/// The Markets and Scans tabs under the page header.
pub(super) fn screener_tabs(
    terminal: &Entity<TerminalApp>,
    selected: ScreenerTab,
    theme: &AerisTheme,
) -> Div {
    let tabs = ScreenerTab::ALL
        .into_iter()
        .enumerate()
        .map(|(index, tab)| {
            let select = terminal.clone();
            Tab::new(("screener_tab", index), theme)
                .selected(tab == selected)
                .aria_label(tr!(panels::SCREENER_SHOW_TAB, tab = tab.label()))
                .on_click(move |_, window, cx| {
                    select.update(cx, |terminal, terminal_cx| {
                        terminal.show_screener_tab(tab, window, terminal_cx);
                    });
                })
                .child(tab.label())
        });
    div().w_full().flex_none().flex().items_center().child(
        TabList::new("screener_tabs", tr!(panels::SCREENER_TABS_LABEL), theme).children(tabs),
    )
}

/// The Scans tab body: the library beside the builder and the running scan's matches.
pub(super) fn market_scans_tab(
    terminal: &Entity<TerminalApp>,
    screener: &MarketScreener,
    theme: &AerisTheme,
) -> AnyElement {
    let scans = &screener.scans;
    if scans.provider().is_none() {
        return note(div().child(tr!(panels::SCAN_NO_PROVIDER)), theme);
    }
    let ui = Ui {
        terminal,
        scans,
        theme,
    };
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .gap(px(SECTION_GAP))
        .child(library(ui))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(px(SECTION_GAP))
                .child(builder(ui))
                .child(results(ui))
                .child(results_footer(screener, theme)),
        )
        .into_any_element()
}

/// Centered secondary text in place of content.
fn note(content: impl IntoElement, theme: &AerisTheme) -> AnyElement {
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(gpui_color(theme.colors.text_secondary))
        .child(content)
        .into_any_element()
}

fn library(ui: Ui<'_>) -> Div {
    let theme = ui.theme;
    let colors = theme.colors;
    let scans = ui.scans;
    let new_terminal = ui.terminal.clone();
    let new = Button::new("scan_new", theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Sm)
        .icon(header_icon(HugeIcon::Add))
        .aria_label(tr!(panels::SCAN_NEW))
        .tooltip(TooltipSpec::new(tr!(panels::SCAN_NEW), theme).show_delay(TOOLTIP_OPEN_DELAY))
        .on_click(move |_, window, cx| {
            new_terminal.update(cx, |terminal, terminal_cx| {
                terminal.new_market_scan(window, terminal_cx);
            });
        });
    let list = if scans.saved().is_empty() {
        div()
            .px_3()
            .text_xs()
            .text_color(gpui_color(colors.text_muted))
            .child(tr!(panels::SCAN_LIBRARY_EMPTY))
            .into_any_element()
    } else {
        let entries = scans.saved().iter().enumerate().map(|(index, scan)| {
            let open = ui.terminal.clone();
            Tab::new(("saved_scan", index), theme)
                .sidebar()
                .selected(scans.selected() == Some(index))
                .gap_2()
                .aria_label(tr!(panels::SCAN_OPEN_SAVED, name = scan.name))
                .on_click(move |_, window, cx| {
                    open.update(cx, |terminal, terminal_cx| {
                        terminal.select_saved_market_scan(index, window, terminal_cx);
                    });
                })
                .child(div().flex_1().min_w_0().truncate().child(scan.name.clone()))
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(gpui_color(colors.text_muted))
                        .child(scan.definition.timeframe.label()),
                )
        });
        TabList::sidebar("saved_scans", tr!(panels::SCAN_LIBRARY_TITLE))
            .children(entries)
            .into_any_element()
    };
    let count = div()
        .text_xs()
        .text_color(gpui_color(colors.text_muted))
        .map(numeric_text)
        .child(format!("{}/{MAXIMUM_SAVED_SCANS}", scans.saved().len()));
    div()
        .w(px(LIBRARY_WIDTH))
        .flex_none()
        .min_h_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(platform_font_weight(TypographyRole::Emphasis))
                        .text_color(gpui_color(colors.text_default))
                        .child(tr!(panels::SCAN_LIBRARY_TITLE)),
                )
                .child(div().flex().items_center().gap_1().child(count).child(new)),
        )
        .child(
            div()
                .id("saved_scans_scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(list),
        )
}

/// A click handler that applies one edit of the scan state.
fn edit_scan(
    terminal: &Entity<TerminalApp>,
    change: impl Fn(&mut MarketScan) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let terminal = terminal.clone();
    move |_, window, cx| {
        terminal.update(cx, |terminal, terminal_cx| {
            terminal.edit_market_scan(window, terminal_cx, &change);
        });
    }
}

/// A builder button that opens `menu`; callers add its label or icon.
fn menu_trigger(ui: Ui<'_>, id: &str, menu: ScanMenu) -> Button {
    let toggle = ui.terminal.clone();
    Button::new(SharedString::from(format!("{id}_button")), ui.theme)
        .variant(ButtonVariant::Outline)
        .open(ui.scans.menu() == Some(menu))
        .on_press(move |_, window, cx| {
            toggle.update(cx, |terminal, terminal_cx| {
                terminal.toggle_market_scan_menu(menu, window, terminal_cx);
            });
        })
}

/// `trigger` with the rows `fill` adds while `menu` is open.
fn menu_anchor(
    ui: Ui<'_>,
    (id, menu): (&str, ScanMenu),
    trigger: Button,
    fill: impl FnOnce(MenuPanel) -> MenuPanel,
) -> MenuAnchor {
    let panel = (ui.scans.menu() == Some(menu)).then(|| {
        fill(
            MenuPanel::new(
                SharedString::from(format!("{id}_menu")),
                MenuPlacement::Anchored {
                    side: MenuSide::Below,
                    align: MenuAlign::Start,
                },
                ui.theme,
            )
            .width(px(MENU_WIDTH))
            .max_height(px(MENU_MAX_HEIGHT)),
        )
    });
    let dismiss = ui.terminal.clone();
    MenuAnchor::new(SharedString::from(format!("{id}_anchor")), trigger)
        .menu(panel)
        .on_dismiss(move |_, cx| {
            dismiss.update(cx, TerminalApp::close_market_scan_menu);
        })
}

/// A menu that sets one value of the draft with `choose`.
fn choice_menu<T: Copy + PartialEq + 'static>(
    ui: Ui<'_>,
    (id, menu): (&str, ScanMenu),
    (label, aria): (String, String),
    choices: impl IntoIterator<Item = (T, String)>,
    current: T,
    choose: impl Fn(&mut ScanDraft, T) -> bool + Copy + 'static,
) -> MenuAnchor {
    let trigger = menu_trigger(ui, id, menu)
        .aria_label(aria)
        .label(label)
        .caret(header_icon(HugeIcon::ChevronDown));
    menu_anchor(ui, (id, menu), trigger, |panel| {
        choices
            .into_iter()
            .enumerate()
            .fold(panel, |panel, (index, (value, label))| {
                panel.child(
                    MenuRow::compact(
                        SharedString::from(format!("{id}_choice_{index}")),
                        label,
                        ui.theme,
                    )
                    .checked(value == current)
                    .on_click(edit_scan(ui.terminal, move |scans| {
                        scans.edit(|draft| choose(draft, value));
                    })),
                )
            })
    })
}

fn field_input(ui: Ui<'_>, field: ScanField, width: f32) -> Option<Div> {
    ui.scans.input(field).map(|input| {
        div()
            .w(px(width))
            .h(px(CONTROL_HEIGHT))
            .flex_none()
            .flex()
            .child(Input::new(input).platform(ui.theme).flex_1())
    })
}

fn builder(ui: Ui<'_>) -> Div {
    let theme = ui.theme;
    let colors = theme.colors;
    let draft = ui.scans.draft();
    let add = Button::new("scan_add_condition", theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Sm)
        .icon(header_icon(HugeIcon::Add))
        .label(tr!(panels::SCAN_ADD_CONDITION))
        .disabled(!draft.can_add_condition())
        .on_click(edit_scan(ui.terminal, |scans| {
            scans.edit(ScanDraft::add_condition);
        }));
    let status = ui.scans.status().map(|status| {
        div()
            .min_w_0()
            .truncate()
            .text_xs()
            .text_color(gpui_color(if status.is_problem() {
                colors.text_warning
            } else {
                colors.text_secondary
            }))
            .child(status.message())
    });
    div()
        .w_full()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(BUILDER_GAP))
        .p_3()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border_secondary))
        .child(builder_header(ui))
        .children(
            draft
                .conditions
                .iter()
                .enumerate()
                .map(|(index, condition)| condition_row(ui, index, condition)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(add)
                .children(status),
        )
}

/// The name, the scan settings and the run, save and library actions.
fn builder_header(ui: Ui<'_>) -> Div {
    let theme = ui.theme;
    let draft = ui.scans.draft();
    let timeframe = choice_menu(
        ui,
        ("scan_timeframe", ScanMenu::Timeframe),
        (
            draft.timeframe.label().to_string(),
            tr!(panels::SCAN_TIMEFRAME_ARIA).to_string(),
        ),
        ui.scans
            .intervals()
            .into_iter()
            .map(|interval| (interval, interval.label().to_string())),
        draft.timeframe,
        |draft, timeframe| {
            draft.timeframe = timeframe;
            true
        },
    );
    let match_mode = choice_menu(
        ui,
        ("scan_match_mode", ScanMenu::MatchMode),
        (
            tr!(match_mode_label(draft.match_mode)).to_string(),
            tr!(panels::SCAN_MATCH_ARIA).to_string(),
        ),
        MATCH_MODES.map(|mode| (mode, tr!(match_mode_label(mode)).to_string())),
        draft.match_mode,
        |draft, mode| {
            draft.match_mode = mode;
            true
        },
    );
    let universe = choice_menu(
        ui,
        ("scan_universe", ScanMenu::Universe),
        (
            tr!(panels::SCAN_UNIVERSE_TOP, count = draft.universe_limit),
            tr!(panels::SCAN_UNIVERSE_ARIA).to_string(),
        ),
        SCAN_UNIVERSE_CHOICES.map(|count| (count, tr!(panels::SCAN_UNIVERSE_TOP, count = count))),
        draft.universe_limit,
        |draft, count| {
            draft.universe_limit = count;
            true
        },
    );
    let run_terminal = ui.terminal.clone();
    let run = Button::new("scan_run", theme)
        .variant(ButtonVariant::Primary)
        .icon(header_icon(HugeIcon::Play))
        .label(tr!(panels::SCAN_RUN))
        .on_click(move |_, _, cx| {
            run_terminal.update(cx, TerminalApp::run_market_scan);
        });
    let save_terminal = ui.terminal.clone();
    let save = Button::new("scan_save", theme)
        .variant(ButtonVariant::Outline)
        .label(tr!(panels::SCAN_SAVE))
        .on_click(move |_, window, cx| {
            save_terminal.update(cx, |terminal, terminal_cx| {
                terminal.save_market_scan(window, terminal_cx);
            });
        });
    div()
        .w_full()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .children(field_input(ui, ScanField::Name, NAME_WIDTH))
        .child(timeframe)
        .child(match_mode)
        .child(universe)
        .child(div().flex_1())
        .child(run)
        .child(save)
        .child(actions_menu(ui))
}

/// Duplicate and the two-step delete of the selected saved scan.
fn actions_menu(ui: Ui<'_>) -> MenuAnchor {
    let theme = ui.theme;
    let scans = ui.scans;
    let trigger = menu_trigger(ui, "scan_actions", ScanMenu::Actions)
        .variant(ButtonVariant::Ghost)
        .icon(header_icon(HugeIcon::More))
        .aria_label(tr!(panels::SCAN_MORE_ACTIONS));
    let duplicate = ui.terminal.clone();
    let delete = ui.terminal.clone();
    menu_anchor(ui, ("scan_actions", ScanMenu::Actions), trigger, |panel| {
        panel
            .child(
                MenuRow::compact("scan_duplicate", tr!(panels::SCAN_DUPLICATE), theme)
                    .leading_icon(header_icon(HugeIcon::Copy))
                    .on_click(move |_, window, cx| {
                        duplicate.update(cx, |terminal, terminal_cx| {
                            terminal.duplicate_market_scan(window, terminal_cx);
                        });
                    }),
            )
            .child(
                MenuRow::compact(
                    "scan_delete",
                    if scans.confirm_delete() {
                        tr!(panels::SCAN_CONFIRM_DELETE)
                    } else {
                        tr!(panels::SCAN_DELETE)
                    },
                    theme,
                )
                .leading_icon(header_icon(HugeIcon::Trash))
                .destructive(true)
                .disabled(scans.selected().is_none())
                .on_click(move |_, window, cx| {
                    delete.update(cx, |terminal, terminal_cx| {
                        terminal.delete_market_scan(window, terminal_cx);
                    });
                }),
            )
    })
}

fn condition_row(ui: Ui<'_>, index: usize, condition: &ConditionDraft) -> Div {
    let colors = ui.theme.colors;
    let number = index + 1;
    let comparison = choice_menu(
        ui,
        (
            &format!("scan_comparison_{index}"),
            ScanMenu::Comparison(index),
        ),
        (
            tr!(comparison_label(condition.comparison)).to_string(),
            tr!(panels::SCAN_COMPARISON_ARIA, number = number),
        ),
        COMPARISONS.map(|comparison| (comparison, tr!(comparison_label(comparison)).to_string())),
        condition.comparison,
        move |draft, comparison| draft.set_comparison(index, comparison),
    );
    let remove = Button::new(SharedString::from(format!("scan_remove_{index}")), ui.theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Sm)
        .icon(header_icon(HugeIcon::Close))
        .aria_label(tr!(panels::SCAN_REMOVE_CONDITION, number = number))
        .disabled(ui.scans.draft().conditions.len() <= 1)
        .on_click(edit_scan(ui.terminal, move |scans| {
            scans.edit(|draft| draft.remove_condition(index));
        }));
    div()
        .w_full()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(CONDITION_NUMBER_WIDTH))
                .flex_none()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .map(numeric_text)
                .child(number.to_string()),
        )
        .child(operand_controls(ui, index, ScanSide::Left, &condition.left))
        .child(comparison)
        .child(operand_controls(
            ui,
            index,
            ScanSide::Right,
            &condition.right,
        ))
        .when(condition.comparison == ScanComparison::Between, |row| {
            row.child(
                div()
                    .text_xs()
                    .text_color(gpui_color(colors.text_secondary))
                    .child(tr!(panels::SCAN_CONDITION_AND)),
            )
            .child(operand_controls(
                ui,
                index,
                ScanSide::Upper,
                &condition.upper,
            ))
        })
        .child(div().flex_1())
        .child(remove)
}

const fn side_index(side: ScanSide) -> usize {
    match side {
        ScanSide::Left => 0,
        ScanSide::Right => 1,
        ScanSide::Upper => 2,
    }
}

/// The menu choosing what one side of a condition reads, then the study line it reads and
/// that kind's inputs.
fn operand_controls(ui: Ui<'_>, condition: usize, side: ScanSide, operand: &OperandDraft) -> Div {
    let colors = ui.theme.colors;
    let id = format!("scan_operand_{condition}_{}", side_index(side));
    let parameters = match operand.kind {
        OperandKind::Study(study) => study.descriptor().parameters,
        _ => &[],
    };
    let label = |text: &'static str| {
        div()
            .text_xs()
            .text_color(gpui_color(colors.text_muted))
            .child(text)
    };
    let parts = operand.kind.parts().into_iter().map(|part| {
        let field = ScanField::Operand {
            condition,
            side,
            part,
        };
        let (name, width) = match part {
            OperandPart::Value => (None, NUMBER_WIDTH),
            OperandPart::Parameter(index) => (
                parameters
                    .get(index)
                    .map(|parameter| parameter_label(parameter.name)),
                PART_WIDTH,
            ),
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .children(name.map(label))
            .children(field_input(ui, field, width))
    });
    let switches = parameters
        .iter()
        .enumerate()
        .filter(|(_, parameter)| parameter.kind == ScanStudyParameterKind::Toggle)
        .map(|(index, parameter)| {
            let name = parameter_label(parameter.name);
            let terminal = ui.terminal.clone();
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    Switch::new(
                        SharedString::from(format!("{id}_switch_{index}")),
                        name,
                        operand.toggle(index),
                        ui.theme,
                    )
                    .on_change(move |on, _, window, cx| {
                        terminal.update(cx, |terminal, terminal_cx| {
                            terminal.edit_market_scan(window, terminal_cx, |scans| {
                                scans.edit(|draft| {
                                    draft.set_operand_toggle(condition, side, (index, on))
                                });
                            });
                        });
                    }),
                )
                .child(label(name))
        });
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(operand_menu(ui, &id, (condition, side), operand))
        .children(line_menu(ui, &id, (condition, side), operand))
        .children(parts)
        .children(switches)
}

/// The grouped, searchable menu of everything a side can read.
fn operand_menu(
    ui: Ui<'_>,
    id: &str,
    (condition, side): (usize, ScanSide),
    operand: &OperandDraft,
) -> MenuAnchor {
    let theme = ui.theme;
    let colors = theme.colors;
    let number = condition + 1;
    let aria = match side {
        ScanSide::Left => tr!(panels::SCAN_MEASURE_ARIA, number = number),
        ScanSide::Right => tr!(panels::SCAN_TARGET_ARIA, number = number),
        ScanSide::Upper => tr!(panels::SCAN_UPPER_ARIA, number = number),
    };
    let menu = ScanMenu::Operand(condition, side);
    let trigger = menu_trigger(ui, id, menu)
        .aria_label(aria)
        .label(operand.kind.label())
        .caret(header_icon(HugeIcon::ChevronDown));
    let current = operand.kind;
    menu_anchor(ui, (id, menu), trigger, |panel| {
        let mut panel = panel
            .width(px(OPERAND_MENU_WIDTH))
            .max_height(px(OPERAND_MENU_MAX_HEIGHT));
        if let Some(input) = ui.scans.operand_search_input() {
            panel = panel.header(
                div()
                    .p_2()
                    .border_b(platform_border_width(theme))
                    .border_color(gpui_color(colors.border_secondary))
                    .child(Input::new(input).platform(theme)),
            );
        }
        let query = ui.scans.operand_search_query();
        let groups = operand_groups(side, query);
        if groups.is_empty() {
            return panel.child(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(gpui_color(colors.text_muted))
                    .child(tr!(panels::SCAN_STUDY_SEARCH_EMPTY, query = query.trim())),
            );
        }
        let mut row = 0_usize;
        for group in groups {
            panel = panel.child(chrome_menu_group_heading(
                family_label(group.family),
                &colors,
            ));
            for kind in group.kinds {
                let choice = MenuRow::compact(
                    SharedString::from(format!("{id}_choice_{row}")),
                    kind.label(),
                    theme,
                )
                .checked(kind == current)
                .on_click(edit_scan(ui.terminal, move |scans| {
                    scans.edit(|draft| draft.set_operand_kind(condition, side, kind));
                }));
                panel = panel.child(match kind.description() {
                    Some(description) => choice.description(description),
                    None => choice,
                });
                row += 1;
            }
        }
        panel
    })
}

/// The line a multi-line study operand reads, such as MACD's signal line.
fn line_menu(
    ui: Ui<'_>,
    id: &str,
    (condition, side): (usize, ScanSide),
    operand: &OperandDraft,
) -> Option<MenuAnchor> {
    let OperandKind::Study(study) = operand.kind else {
        return None;
    };
    let outputs = study.descriptor().outputs;
    if outputs.len() < 2 {
        return None;
    }
    let name = |index: usize| {
        outputs
            .get(index)
            .and_then(|output| line_label(output.name))
            .unwrap_or_default()
            .to_string()
    };
    Some(choice_menu(
        ui,
        (&format!("{id}_line"), ScanMenu::Line(condition, side)),
        (
            name(operand.output),
            tr!(panels::SCAN_LINE_ARIA, number = condition + 1),
        ),
        (0..outputs.len()).map(|index| (index, name(index))),
        operand.output,
        move |draft, output| draft.set_operand_output(condition, side, output),
    ))
}

struct ResultColumn {
    column: ScanResultColumn,
    label: String,
    /// Fixed width of a numeric column; the market column takes the remaining space.
    width: Option<f32>,
}

fn result_columns(run: &ScanRun) -> Vec<ResultColumn> {
    let mut columns = vec![
        ResultColumn {
            column: ScanResultColumn::Market,
            label: tr!(panels::SCREENER_COLUMN_MARKET).to_string(),
            width: None,
        },
        ResultColumn {
            column: ScanResultColumn::Close,
            label: tr!(panels::SCAN_COLUMN_CLOSE).to_string(),
            width: Some(CLOSE_WIDTH),
        },
        ResultColumn {
            column: ScanResultColumn::Change,
            label: tr!(panels::SCAN_COLUMN_CHANGE).to_string(),
            width: Some(CHANGE_WIDTH),
        },
        ResultColumn {
            column: ScanResultColumn::Volume,
            label: tr!(panels::SCAN_COLUMN_VOLUME).to_string(),
            width: Some(VOLUME_WIDTH),
        },
    ];
    columns.extend(
        run.columns()
            .iter()
            .enumerate()
            .map(|(index, label)| ResultColumn {
                column: ScanResultColumn::Condition(index),
                label: label.clone(),
                width: Some(VALUE_COLUMN_WIDTH),
            }),
    );
    columns
}

/// A result cell; every cell but the row's last carries the column divider.
fn result_cell(column: &ResultColumn, last: bool, theme: &AerisTheme) -> Div {
    let cell = table_cell(theme, !last);
    match column.width {
        Some(width) => cell.w(px(width)).flex_none().justify_end(),
        None => cell.min_w(px(MARKET_MIN_WIDTH)).flex_1(),
    }
}

fn results_frame(theme: &AerisTheme) -> Div {
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border(px(theme.dimensions.border_width))
        .border_color(gpui_color(theme.colors.border_secondary))
        .overflow_hidden()
}

fn results(ui: Ui<'_>) -> AnyElement {
    let Some(run) = ui.scans.run() else {
        return results_frame(ui.theme)
            .child(note(div().child(tr!(panels::SCAN_IDLE)), ui.theme))
            .into_any_element();
    };
    let columns = Rc::new(result_columns(run));
    results_frame(ui.theme)
        .child(results_header(ui, &columns))
        .child(results_body(ui, run, columns))
        .into_any_element()
}

fn results_header(ui: Ui<'_>, columns: &[ResultColumn]) -> Div {
    let colors = ui.theme.colors;
    let last = columns.len().saturating_sub(1);
    table_row_frame(TABLE_HEADER_HEIGHT)
        // GPUI clips children to a rectangle, so the filled header carries the container's
        // rounding itself rather than painting over the rounded border.
        .rounded_t(table_inner_radius(ui.theme))
        .bg(gpui_color(colors.surface_secondary))
        .border_b(px(ui.theme.dimensions.border_width))
        .border_color(gpui_color(colors.border_secondary))
        .text_xs()
        .text_color(gpui_color(colors.text_secondary))
        .child(rank_cell(ui.theme).child("#"))
        .children(
            columns
                .iter()
                .enumerate()
                .map(|(index, column)| header_cell(ui, index, column, index == last)),
        )
}

fn header_cell(ui: Ui<'_>, index: usize, column: &ResultColumn, last: bool) -> Div {
    let colors = ui.theme.colors;
    let sort = ui.scans.sort();
    let active = sort.column == column.column;
    let indicator = active.then(|| {
        header_icon(HugeIcon::ChevronDown)
            .with_size(px(SORT_ICON_SIZE))
            .color(gpui_color(colors.text_default))
            .when(!sort.descending, |icon| icon.rotate(0.5))
    });
    let direction = if active && sort.descending {
        tr!(panels::SCREENER_DIRECTION_DESCENDING)
    } else {
        tr!(panels::SCREENER_DIRECTION_ASCENDING)
    };
    let sort_terminal = ui.terminal.clone();
    let target = column.column;
    let numeric = column.width.is_some();
    let label = div().min_w_0().truncate().child(column.label.clone());
    result_cell(column, last, ui.theme).child(
        div()
            .id(("market_scan_sort", index))
            .h_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .role(Role::Button)
            .aria_label(if active {
                tr!(
                    panels::SCREENER_SORTED_ARIA,
                    label = column.label,
                    direction = direction
                )
            } else {
                tr!(panels::SCREENER_SORT_BY, label = column.label)
            })
            .when(active, |cell| {
                cell.text_color(gpui_color(colors.text_default))
            })
            .hover(move |cell| cell.text_color(gpui_color(colors.text_default)))
            .on_click(move |_, _, cx| {
                sort_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.sort_market_scan(target, terminal_cx);
                });
            })
            .map(|cell| {
                // Numeric columns are right-aligned, so their indicator leads the label.
                if numeric {
                    cell.children(indicator).child(label)
                } else {
                    cell.child(label).children(indicator)
                }
            }),
    )
}

fn results_body(ui: Ui<'_>, run: &ScanRun, columns: Rc<Vec<ResultColumn>>) -> AnyElement {
    let colors = ui.theme.colors;
    let rows = ui.scans.rows();
    if rows.is_empty() {
        let content = if run.scanning() && !run.completed() {
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    Loader::new("market_scan_loader", header_icon(HugeIcon::Loader))
                        .color(gpui_color(colors.text_secondary)),
                )
                .child(tr!(panels::SCAN_WAITING))
        } else {
            div().child(tr!(panels::SCAN_NO_MATCHES))
        };
        return note(content, ui.theme);
    }
    let list_terminal = ui.terminal.clone();
    let list_theme = *ui.theme;
    let list_rows = Rc::clone(&rows);
    let logo = ui.scans.exchange_logo();
    let scroll = ui.scans.scroll.clone();
    let scrollbar = scroll.0.borrow().base_handle.clone();
    div()
        .relative()
        .flex_1()
        .min_h_0()
        .w_full()
        .child(
            uniform_list("market_scan_rows", rows.len(), move |range, _, _| {
                let last = list_rows.len().saturating_sub(1);
                range
                    .filter_map(|index| {
                        Some(result_row(
                            &list_terminal,
                            (index, index == last),
                            list_rows.get(index)?,
                            (&columns, logo),
                            &list_theme,
                        ))
                    })
                    .collect::<Vec<_>>()
            })
            .track_scroll(&scroll)
            .size_full(),
        )
        .child(ThinScrollbar::new(
            &scrollbar,
            gpui_color(colors.text_secondary),
        ))
        .into_any_element()
}

fn result_row(
    terminal: &Entity<TerminalApp>,
    (index, last): (usize, bool),
    row: &ScanResultRow,
    (columns, logo): (&[ResultColumn], Option<assets::ExchangeLogo>),
    theme: &AerisTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let open_terminal = terminal.clone();
    let open_row = row.clone();
    let final_column = columns.len().saturating_sub(1);
    let cell = |position: usize| {
        columns
            .get(position)
            .map(|column| result_cell(column, position == final_column, theme))
    };
    let market = cell(0).map(|cell| {
        cell.gap_2()
            .children(logo.map(|logo| exchange_mark(logo, px(EXCHANGE_LOGO_SIZE), false, &colors)))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(platform_font_weight(TypographyRole::Emphasis))
                    .child(row.display_symbol.clone()),
            )
    });
    let values = row.values.iter().enumerate().filter_map(|(offset, value)| {
        Some(cell(LEADING_RESULT_COLUMNS + offset)?.child(value.clone()))
    });
    table_row_frame(ROW_HEIGHT)
        .id(("market_scan_row", index))
        // The final row meets the container's rounded border, which also separates it.
        .map(|frame| {
            if last {
                frame.rounded_b(table_inner_radius(theme))
            } else {
                frame
                    .border_b(px(theme.dimensions.border_width))
                    .border_color(gpui_color(colors.border_subtle))
            }
        })
        .text_sm()
        .text_color(gpui_color(colors.text_default))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(tr!(panels::SCREENER_OPEN_ROW, symbol = row.display_symbol))
        .hover(move |row| row.bg(gpui_color(colors.hover_bg.over(colors.surface))))
        .on_click(move |_, _, cx| {
            open_terminal.update(cx, |terminal, terminal_cx| {
                terminal.open_market_scan_row(&open_row, terminal_cx);
            });
        })
        .on_mouse_down(
            MouseButton::Right,
            row_menu_on_right_click(terminal, &row.symbol, &row.exchange, &row.display_symbol),
        )
        .child(
            rank_cell(theme)
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child((index + 1).to_string()),
        )
        .children(market)
        .children(cell(1).map(|cell| cell.child(row.close.clone())))
        .children(cell(2).map(|cell| {
            cell.text_color(direction_color(row.change_direction, theme))
                .child(row.change.clone())
        }))
        .children(cell(3).map(|cell| cell.child(row.volume.clone())))
        .children(values)
}

/// The run's progress or notice, the opening market and the data caption.
fn results_footer(screener: &MarketScreener, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    let summary = screener.scans.run().map(|run| match run.notice() {
        Some(ScanRunNotice::Busy) => (tr!(panels::SCAN_BUSY).to_string(), colors.text_warning),
        Some(ScanRunNotice::Retrying) => {
            (tr!(panels::SCAN_RETRYING).to_string(), colors.text_warning)
        }
        None => (run_summary(run), colors.text_muted),
    });
    let status = screener
        .pending_selection_status()
        .map(|text| (text, colors.text_secondary))
        .or_else(|| {
            screener
                .message()
                .map(|message| (message.to_string(), colors.text_warning))
        })
        .or_else(|| watchlist_status_line(screener, theme));
    let line = |(text, color): (String, ThemeColor)| {
        div()
            .min_w_0()
            .truncate()
            .text_color(gpui_color(color))
            .child(text)
    };
    div()
        .w_full()
        .h(px(FOOTER_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .text_xs()
        .text_color(gpui_color(colors.text_muted))
        .map(numeric_text)
        .child(div().min_w_0().flex_1().children(summary.map(line)))
        .children(status.map(line))
        .child(div().flex_none().child(tr!(panels::SCAN_CAPTION)))
}

impl TerminalApp {
    pub(super) fn show_screener_tab(
        &mut self,
        tab: ScreenerTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pages.screener.tab() == tab {
            return;
        }
        self.pages.screener.set_tab(tab);
        self.refresh_market_scan_inputs(window, cx);
        self.pages.screener.refresh_if_due(Instant::now());
        cx.notify();
    }

    /// Binds one input to every text the draft shows. Inputs already bound to the draft's
    /// fields keep their text and focus.
    pub(super) fn refresh_market_scan_inputs(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scans = &self.pages.screener.scans;
        let fields = scans.draft().fields();
        if scans.inputs_match(&fields) {
            return;
        }
        let mut inputs = Vec::with_capacity(fields.len());
        for field in fields {
            let value = scans.draft().field(field).unwrap_or_default().to_string();
            let input = cx.new(|input_cx| {
                let input = InputState::new(window, input_cx).default_value(value);
                if field == ScanField::Name {
                    input.placeholder(tr!(panels::SCAN_NAME_PLACEHOLDER))
                } else {
                    input
                }
            });
            cx.subscribe(&input, move |terminal, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    if terminal
                        .pages
                        .screener
                        .scans
                        .edit(|draft| draft.set_field(field, value))
                    {
                        cx.notify();
                    }
                }
            })
            .detach();
            inputs.push((field, input));
        }
        self.pages.screener.scans.set_inputs(inputs);
    }

    /// Applies one builder control's edit and rebinds the inputs to the edited draft, whose
    /// texts the edit may have replaced.
    pub(super) fn edit_market_scan(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut MarketScan),
    ) {
        let scans = &mut self.pages.screener.scans;
        change(scans);
        scans.close_menu();
        scans.clear_inputs();
        self.refresh_market_scan_inputs(window, cx);
        cx.notify();
    }

    pub(super) fn run_market_scan(&mut self, cx: &mut Context<Self>) {
        self.pages.screener.scans.run_draft();
        self.pages.screener.refresh_if_due(Instant::now());
        cx.notify();
    }

    pub(super) fn save_market_scan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changed = self.pages.screener.scans.save();
        self.finish_market_scan_library_action(changed, window, cx);
    }

    pub(super) fn duplicate_market_scan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changed = self.pages.screener.scans.duplicate();
        self.finish_market_scan_library_action(changed, window, cx);
    }

    pub(super) fn delete_market_scan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changed = self.pages.screener.scans.request_delete();
        self.finish_market_scan_library_action(changed, window, cx);
    }

    /// Saves a changed library through the workspace writer and shows the draft as the
    /// action left it.
    fn finish_market_scan_library_action(
        &mut self,
        changed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if changed {
            self.record_layout_change(cx);
        }
        self.pages.screener.scans.clear_inputs();
        self.refresh_market_scan_inputs(window, cx);
        cx.notify();
    }

    pub(super) fn select_saved_market_scan(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pages.screener.scans.select_saved(index) {
            self.pages.screener.scans.clear_inputs();
            self.refresh_market_scan_inputs(window, cx);
            self.pages.screener.refresh_if_due(Instant::now());
        }
        cx.notify();
    }

    pub(super) fn new_market_scan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pages.screener.scans.new_scan();
        self.pages.screener.scans.clear_inputs();
        self.refresh_market_scan_inputs(window, cx);
        cx.notify();
    }

    /// Opens or closes a builder menu; an opened operand menu gets a focused search field.
    pub(super) fn toggle_market_scan_menu(
        &mut self,
        menu: ScanMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scans = &mut self.pages.screener.scans;
        scans.toggle_menu(menu);
        if matches!(scans.menu(), Some(ScanMenu::Operand(..))) {
            let input = cx.new(|input_cx| {
                InputState::new(window, input_cx)
                    .placeholder(tr!(panels::SCAN_STUDY_SEARCH_PLACEHOLDER))
            });
            input.update(cx, |input, input_cx| input.focus(window, input_cx));
            cx.subscribe_in(
                &input,
                window,
                |terminal, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        let query = input.read(cx).value().to_string();
                        if terminal
                            .pages
                            .screener
                            .scans
                            .set_operand_search_query(input, query)
                        {
                            cx.notify();
                        }
                    }
                    InputEvent::PressEnter { .. } => {
                        let scans = &terminal.pages.screener.scans;
                        if scans.operand_search_input() != Some(input) {
                            return;
                        }
                        if let Some((condition, side, kind)) = scans.first_operand_match() {
                            terminal.edit_market_scan(window, cx, |scans| {
                                scans.edit(|draft| draft.set_operand_kind(condition, side, kind));
                            });
                        }
                    }
                    InputEvent::Focus | InputEvent::Blur => {}
                },
            )
            .detach();
            self.pages.screener.scans.set_operand_search(input);
        }
        cx.notify();
    }

    pub(super) fn close_market_scan_menu(&mut self, cx: &mut Context<Self>) {
        let scans = &mut self.pages.screener.scans;
        if scans.menu().is_some() || scans.confirm_delete() {
            scans.close_menu();
            cx.notify();
        }
    }

    pub(super) fn sort_market_scan(&mut self, column: ScanResultColumn, cx: &mut Context<Self>) {
        self.pages.screener.scans.set_sort_column(column);
        cx.notify();
    }

    /// Opens a match on the timeframe the scan found it on.
    pub(super) fn open_market_scan_row(&mut self, row: &ScanResultRow, cx: &mut Context<Self>) {
        let interval = self.pages.screener.scans.run().map(ScanRun::timeframe);
        self.pages
            .screener
            .open_market(&row.symbol, &row.exchange, &row.display_symbol, interval);
        cx.notify();
    }
}
