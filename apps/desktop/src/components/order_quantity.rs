//! The order quantity a surface's order controls share: the order ticket's quantity field and
//! the Buy / quantity / Sell bar under the chart's symbol legend, with its quantity menu. The
//! quantity itself stays in the surface's order entry; this module only edits and shows it.

use super::order_ticket::{dispatch_market, trade_variant};
use super::*;
use gpui::{anchored, deferred};

/// The largest quantity one order entry accepts.
pub(super) const MAXIMUM_ORDER_QUANTITY: u64 = 999;
/// The quantities the chart's quantity menu offers under its custom field.
const QUANTITY_PRESETS: [u64; 8] = [1, 2, 3, 5, 10, 15, 20, 25];
const QUANTITY_MENU_WIDTH: f32 = 168.0;
const QUANTITY_FIELD_HEIGHT: f32 = 28.0;
const TRADE_BAR_BUTTON_WIDTH: f32 = 56.0;
/// Keeps the quantity menu clear of the window edges when it has to slide inside.
const QUANTITY_MENU_WINDOW_MARGIN: f32 = 8.0;

/// A typed quantity, accepted only as a whole number of lots within the order-entry range.
pub(super) fn parse_order_quantity(text: &str) -> Option<u64> {
    text.trim()
        .parse::<u64>()
        .ok()
        .filter(|quantity| (1..=MAXIMUM_ORDER_QUANTITY).contains(quantity))
}

/// The editing state of the surface's quantity controls.
pub(super) struct OrderQuantityControls {
    ticket_input: Entity<InputState>,
    /// The ticket field holds text that is not a valid quantity; the order entry keeps the
    /// last valid one until the field is corrected or left.
    ticket_invalid: bool,
    menu_input: Entity<InputState>,
    menu_invalid: bool,
    menu_open: bool,
    /// Window bounds of the chart bar's quantity trigger from the latest prepaint.
    menu_trigger: Option<Bounds<Pixels>>,
}

impl OrderQuantityControls {
    pub(super) fn new(quantity: u64, window: &mut Window, cx: &mut App) -> Self {
        let ticket_input = cx.new(|input_cx| InputState::new(window, input_cx));
        ticket_input.update(cx, |input, input_cx| {
            input.set_text_align(gpui::TextAlign::Center, input_cx);
            input.set_value(quantity.to_string(), window, input_cx);
        });
        let menu_input =
            cx.new(|input_cx| InputState::new(window, input_cx).placeholder("Custom quantity"));
        Self {
            ticket_input,
            ticket_invalid: false,
            menu_input,
            menu_invalid: false,
            menu_open: false,
            menu_trigger: None,
        }
    }
}

#[cfg(test)]
impl OrderQuantityControls {
    pub(super) const fn ticket_input(&self) -> &Entity<InputState> {
        &self.ticket_input
    }

    pub(super) const fn menu_input(&self) -> &Entity<InputState> {
        &self.menu_input
    }

    pub(super) const fn ticket_invalid(&self) -> bool {
        self.ticket_invalid
    }

    pub(super) const fn menu_invalid(&self) -> bool {
        self.menu_invalid
    }

    pub(super) const fn menu_open(&self) -> bool {
        self.menu_open
    }
}

/// Routes the quantity fields' edits to their surface.
pub(super) fn subscribe_order_quantity_inputs(
    surface: &Entity<WorkspaceSurface>,
    window: &mut Window,
    cx: &mut App,
) {
    let (ticket_input, menu_input) = {
        let controls = &surface.read(cx).order_quantity;
        (controls.ticket_input.clone(), controls.menu_input.clone())
    };
    let ticket_surface = surface.clone();
    window
        .subscribe(
            &ticket_input,
            cx,
            move |_, event: &InputEvent, window, cx| {
                ticket_surface.update(cx, |surface, surface_cx| match event {
                    InputEvent::Change => surface.apply_ticket_quantity_text(surface_cx),
                    // Leaving or submitting the field shows the quantity orders will use.
                    InputEvent::PressEnter { .. } | InputEvent::Blur => {
                        surface.show_order_quantity_in_ticket(window, surface_cx);
                    }
                    InputEvent::Focus => {}
                });
            },
        )
        .detach();
    let menu_surface = surface.clone();
    window
        .subscribe(&menu_input, cx, move |_, event: &InputEvent, window, cx| {
            menu_surface.update(cx, |surface, surface_cx| match event {
                InputEvent::Change => {
                    if surface.order_quantity.menu_invalid {
                        surface.order_quantity.menu_invalid = false;
                        surface_cx.notify();
                    }
                }
                InputEvent::PressEnter { .. } => {
                    surface.submit_order_quantity_menu(window, surface_cx);
                }
                InputEvent::Focus | InputEvent::Blur => {}
            });
        })
        .detach();
}

impl WorkspaceSurface {
    /// Sets the quantity every order control on this surface sends.
    pub(super) fn set_order_quantity(
        &mut self,
        quantity: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.trading_pnl.order_entry.quantity = quantity.clamp(1, MAXIMUM_ORDER_QUANTITY);
        self.show_order_quantity_in_ticket(window, cx);
    }

    fn show_order_quantity_in_ticket(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let quantity = self.trading_pnl.order_entry.quantity;
        self.order_quantity.ticket_invalid = false;
        let input = self.order_quantity.ticket_input.clone();
        if parse_order_quantity(input.read(cx).value().as_ref()) != Some(quantity) {
            input.update(cx, |input, input_cx| {
                input.set_value(quantity.to_string(), window, input_cx);
            });
        }
        cx.notify();
    }

    pub(super) fn apply_ticket_quantity_text(&mut self, cx: &mut Context<Self>) {
        let text = self.order_quantity.ticket_input.read(cx).value();
        match parse_order_quantity(text.as_ref()) {
            Some(quantity) => {
                self.trading_pnl.order_entry.quantity = quantity;
                self.order_quantity.ticket_invalid = false;
            }
            // An emptied field is mid-edit, not an error yet.
            None => self.order_quantity.ticket_invalid = !text.trim().is_empty(),
        }
        cx.notify();
    }

    pub(super) fn toggle_order_quantity_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.order_quantity.menu_open {
            self.close_order_quantity_menu(window, cx);
            return;
        }
        self.order_quantity.menu_open = true;
        self.order_quantity.menu_invalid = false;
        self.order_quantity
            .menu_input
            .update(cx, |input, input_cx| {
                input.set_value("", window, input_cx);
                input.focus(window, input_cx);
            });
        cx.notify();
    }

    pub(super) fn close_order_quantity_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.order_quantity.menu_open {
            return;
        }
        self.order_quantity.menu_open = false;
        self.order_quantity.menu_invalid = false;
        self.focus_keyboard(window, cx);
        cx.notify();
    }

    fn choose_order_quantity(
        &mut self,
        quantity: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_order_quantity(quantity, window, cx);
        self.close_order_quantity_menu(window, cx);
    }

    pub(super) fn submit_order_quantity_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.order_quantity.menu_input.read(cx).value();
        if let Some(quantity) = parse_order_quantity(text.as_ref()) {
            self.choose_order_quantity(quantity, window, cx);
        } else {
            self.order_quantity.menu_invalid = true;
            cx.notify();
        }
    }

    /// Records where the chart bar's quantity trigger is, so its menu opens under it.
    fn record_order_quantity_trigger(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        if self.order_quantity.menu_trigger == Some(bounds) {
            return;
        }
        self.order_quantity.menu_trigger = Some(bounds);
        if self.order_quantity.menu_open {
            cx.notify();
        }
    }
}

/// The order ticket's editable quantity.
pub(super) fn ticket_quantity_field(
    controls: &OrderQuantityControls,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    div().h_full().w(px(64.0)).flex().child(
        Input::new(&controls.ticket_input)
            .platform(theme)
            .invalid(controls.ticket_invalid)
            .flex_1(),
    )
}

/// The Buy / quantity / Sell bar the chart shows under its symbol legend. It reads the
/// surface's order entry and order book each frame and sends through the same market-order
/// path as the order ticket.
pub(super) struct ChartTradeBar {
    surface: WeakEntity<WorkspaceSurface>,
}

/// Puts the surface's trade bar under the symbol legend of a chart the surface now shows.
pub(super) fn attach_chart_trade_bar(
    chart: &Entity<AerisChartView>,
    cx: &mut Context<WorkspaceSurface>,
) {
    let surface = cx.weak_entity();
    let trade_bar = cx.new(|_| ChartTradeBar { surface });
    chart.update(cx, |chart, _| {
        chart.set_legend_accessory(Some(trade_bar.into()));
    });
}

impl Render for ChartTradeBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(surface) = self.surface.upgrade() else {
            return div().into_any_element();
        };
        let state = surface.read(cx);
        let theme = state.theme;
        let trading = &state.trading_pnl;
        let frame = state.order_book.read(cx).frame().cloned();
        let account = trading
            .order_entry
            .selected_account_id
            .as_ref()
            .map(|id| id.as_str().to_string());
        let ready =
            frame.is_some() && account.is_some() && selected_account_lock_reason(trading).is_none();
        let order = MarketOrderTarget {
            frame,
            account,
            quantity: trading.order_entry.quantity,
            ready,
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(trade_bar_order_button(
                "chart_trade_buy",
                "Buy",
                aeris_trading::OrderSide::Buy,
                &order,
                &theme,
            ))
            .child(quantity_trigger(
                surface.clone(),
                order.quantity,
                state.order_quantity.menu_open,
                &theme,
            ))
            .child(trade_bar_order_button(
                "chart_trade_sell",
                "Sell",
                aeris_trading::OrderSide::Sell,
                &order,
                &theme,
            ))
            .into_any_element()
    }
}

/// What a bar order sends, captured at render like the order ticket's buttons.
struct MarketOrderTarget {
    frame: Option<aeris_market_data::OrderBookFrame>,
    account: Option<String>,
    quantity: u64,
    ready: bool,
}

fn trade_bar_order_button(
    id: &'static str,
    label: &'static str,
    side: aeris_trading::OrderSide,
    order: &MarketOrderTarget,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let frame = order.frame.clone();
    let account = order.account.clone();
    let quantity = order.quantity;
    div().w(px(TRADE_BAR_BUTTON_WIDTH)).child(
        Button::new(id, theme)
            .variant(trade_variant(side))
            .button_size(ButtonSize::Sm)
            .strong()
            .full_width()
            .label(label)
            .disabled(!order.ready)
            .on_click(move |_, _, cx| {
                if let Some(frame) = frame.as_ref() {
                    dispatch_market(frame, account.clone(), quantity, side, cx);
                }
            }),
    )
}

fn quantity_trigger(
    surface: Entity<WorkspaceSurface>,
    quantity: u64,
    open: bool,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let bounds_surface = surface.clone();
    div()
        .relative()
        .flex_none()
        .child(
            canvas(
                move |bounds, _, cx| {
                    bounds_surface.update(cx, |surface, surface_cx| {
                        surface.record_order_quantity_trigger(bounds, surface_cx);
                    });
                },
                |_, (), _, _| {},
            )
            .absolute()
            .inset_0(),
        )
        .child(
            Button::new("chart_trade_quantity", theme)
                .variant(ButtonVariant::Outline)
                .button_size(ButtonSize::Sm)
                .trigger()
                .open(open)
                .aria_label("Order quantity")
                .label(quantity.to_string())
                .caret(header_icon(HugeIcon::ChevronDown))
                .on_click(move |_, window, cx| {
                    surface.update(cx, |surface, surface_cx| {
                        surface.toggle_order_quantity_menu(window, surface_cx);
                    });
                }),
        )
}

/// The open quantity menu of a pane's chart bar: a custom field above the preset quantities.
/// It renders beside the chart rather than inside it, so typing in its field never reaches
/// chart keyboard input.
pub(super) fn order_quantity_menu_layer(
    surface_entity: &Entity<WorkspaceSurface>,
    surface: &WorkspaceSurface,
    theme: &AerisTheme,
) -> Option<AnyElement> {
    let controls = &surface.order_quantity;
    if !controls.menu_open {
        return None;
    }
    let trigger = controls.menu_trigger?;
    let quantity = surface.trading_pnl.order_entry.quantity;
    let submit = surface_entity.clone();
    let mut header = div().flex().flex_col().gap_1().p_1().child(
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div().flex_1().h(px(QUANTITY_FIELD_HEIGHT)).flex().child(
                    Input::new(&controls.menu_input)
                        .platform(theme)
                        .invalid(controls.menu_invalid)
                        .flex_1(),
                ),
            )
            .child(
                Button::new("chart_trade_quantity_apply", theme)
                    .variant(ButtonVariant::Secondary)
                    .label("Set")
                    .on_click(move |_, window, cx| {
                        submit.update(cx, |surface, surface_cx| {
                            surface.submit_order_quantity_menu(window, surface_cx);
                        });
                    }),
            ),
    );
    if controls.menu_invalid {
        header = header.child(
            div()
                .px_1()
                .text_xs()
                .text_color(gpui_color(theme.colors.danger))
                .child(format!(
                    "Enter a whole number from 1 to {MAXIMUM_ORDER_QUANTITY}"
                )),
        );
    }
    let panel = MenuPanel::new("chart_trade_quantity_menu", MenuPlacement::InFlow, theme)
        .width(px(QUANTITY_MENU_WIDTH))
        .animate_from(PopupAnimationOrigin::TOP_LEFT)
        .header(header)
        .children(QUANTITY_PRESETS.map(|preset| {
            let choose = surface_entity.clone();
            MenuRow::compact(
                ("chart_trade_quantity_preset", preset),
                preset.to_string(),
                theme,
            )
            .checked(quantity == preset)
            .on_click(move |_, window, cx| {
                choose.update(cx, |surface, surface_cx| {
                    surface.choose_order_quantity(preset, window, surface_cx);
                });
            })
        }));
    let dismiss = surface_entity.clone();
    let escape = surface_entity.clone();
    let menu = div()
        .id("chart_trade_quantity_menu_layer")
        // A press on the trigger toggles the menu itself; any other press outside closes it.
        .on_mouse_down_out(move |event, window, cx| {
            dismiss.update(cx, |surface, surface_cx| {
                if surface
                    .order_quantity
                    .menu_trigger
                    .is_some_and(|trigger| trigger.contains(&event.position))
                {
                    return;
                }
                surface.close_order_quantity_menu(window, surface_cx);
            });
        })
        .on_key_down(move |event, window, cx| {
            if event.keystroke.key == "escape" {
                escape.update(cx, |surface, surface_cx| {
                    surface.close_order_quantity_menu(window, surface_cx);
                });
                cx.stop_propagation();
            }
        })
        .child(panel);
    Some(
        deferred(
            anchored()
                .position(point(trigger.left(), trigger.bottom() + ANCHOR_GAP))
                .snap_to_window_with_margin(px(QUANTITY_MENU_WINDOW_MARGIN))
                .child(menu),
        )
        .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_quantities_must_be_whole_lots_within_the_order_entry_range() {
        assert_eq!(parse_order_quantity("7"), Some(7));
        assert_eq!(parse_order_quantity(" 25 "), Some(25));
        assert_eq!(
            parse_order_quantity(&MAXIMUM_ORDER_QUANTITY.to_string()),
            Some(MAXIMUM_ORDER_QUANTITY)
        );
        for rejected in ["", "0", "-3", "2.5", "1000", "ten"] {
            assert_eq!(parse_order_quantity(rejected), None, "{rejected:?}");
        }
    }

    #[test]
    fn every_preset_is_an_accepted_quantity() {
        for preset in QUANTITY_PRESETS {
            assert_eq!(parse_order_quantity(&preset.to_string()), Some(preset));
        }
    }
}
