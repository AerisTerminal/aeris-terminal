//! The order ticket in the order entry panel: the symbol and its position, the quantity, the
//! broker and account, the buy and sell order grid, and an optional stop-and-target bracket.
//! Market orders use the solid buy and sell fills, resting orders the tinted ones.

use super::*;
use gpui::ClickEvent;

const FIELD_HEIGHT: f32 = 28.0;
const GAP: f32 = 4.0;
const STEP_BUTTON_SIZE: f32 = 18.0;
const SELECT_MENU_MAX_HEIGHT: f32 = 240.0;
/// The largest tick distance a ticket field accepts.
const MAXIMUM_TICKS: u32 = 999;
const STOP_OFFSET_REQUIRED: &str = "Turn on Ticks diff to set how far from the market a stop rests";

/// The ticket's resting-order offset and bracket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TicketSettings {
    /// While on, LMT and STP orders rest `offset_ticks` away from the touch.
    pub(super) offset_enabled: bool,
    pub(super) offset_ticks: u32,
    /// While on, every ticket order carries a stop and a target.
    pub(super) bracket_enabled: bool,
    pub(super) stop_ticks: u32,
    pub(super) target_ticks: u32,
}

impl Default for TicketSettings {
    fn default() -> Self {
        Self {
            offset_enabled: false,
            offset_ticks: 0,
            bracket_enabled: false,
            stop_ticks: 8,
            target_ticks: 16,
        }
    }
}

impl TicketSettings {
    const fn offset(self) -> u32 {
        if self.offset_enabled {
            self.offset_ticks
        } else {
            0
        }
    }

    const fn bracket(self) -> Option<aeris_desktop::trading::TicketBracket> {
        if self.bracket_enabled {
            Some(aeris_desktop::trading::TicketBracket {
                stop_ticks: self.stop_ticks,
                target_ticks: self.target_ticks,
            })
        } else {
            None
        }
    }
}

/// The ticket's typed tick distances.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TicketNumber {
    Offset,
    Stop,
    Target,
}

impl TicketNumber {
    const ALL: [Self; 3] = [Self::Offset, Self::Stop, Self::Target];

    const fn index(self) -> usize {
        match self {
            Self::Offset => 0,
            Self::Stop => 1,
            Self::Target => 2,
        }
    }

    /// A zero offset joins the touch; a bracket leg needs a real distance.
    const fn minimum(self) -> u32 {
        match self {
            Self::Offset => 0,
            Self::Stop | Self::Target => 1,
        }
    }

    fn parse(self, text: &str) -> Option<u32> {
        text.trim()
            .parse::<u32>()
            .ok()
            .filter(|ticks| (self.minimum()..=MAXIMUM_TICKS).contains(ticks))
    }

    const fn value(self, settings: &TicketSettings) -> u32 {
        match self {
            Self::Offset => settings.offset_ticks,
            Self::Stop => settings.stop_ticks,
            Self::Target => settings.target_ticks,
        }
    }

    const fn set(self, settings: &mut TicketSettings, ticks: u32) {
        match self {
            Self::Offset => settings.offset_ticks = ticks,
            Self::Stop => settings.stop_ticks = ticks,
            Self::Target => settings.target_ticks = ticks,
        }
    }
}

/// The ticket's select menus; at most one is open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TicketMenu {
    Broker,
    Account,
    BracketMode,
}

/// The ticket's editing state: its tick fields and which menu is open. The values themselves
/// stay in the surface's order entry.
pub(super) struct OrderTicketControls {
    inputs: [Entity<InputState>; 3],
    invalid: [bool; 3],
    open_menu: Option<TicketMenu>,
}

impl OrderTicketControls {
    pub(super) fn new(settings: TicketSettings, window: &mut Window, cx: &mut App) -> Self {
        let inputs = TicketNumber::ALL.map(|number| {
            let input = cx.new(|input_cx| InputState::new(window, input_cx));
            input.update(cx, |input, input_cx| {
                input.set_text_align(gpui::TextAlign::Right, input_cx);
                input.set_value(number.value(&settings).to_string(), window, input_cx);
            });
            input
        });
        Self {
            inputs,
            invalid: [false; 3],
            open_menu: None,
        }
    }

    const fn input(&self, number: TicketNumber) -> &Entity<InputState> {
        &self.inputs[number.index()]
    }

    const fn invalid(&self, number: TicketNumber) -> bool {
        self.invalid[number.index()]
    }
}

/// Routes the ticket's tick fields' edits to their surface.
pub(super) fn subscribe_order_ticket_inputs(
    surface: &Entity<WorkspaceSurface>,
    window: &mut Window,
    cx: &mut App,
) {
    for number in TicketNumber::ALL {
        let input = surface.read(cx).order_ticket.input(number).clone();
        let target = surface.clone();
        window
            .subscribe(&input, cx, move |_, event: &InputEvent, window, cx| {
                target.update(cx, |surface, surface_cx| match event {
                    InputEvent::Change => surface.apply_ticket_number_text(number, surface_cx),
                    // Leaving or submitting the field shows the value orders will use.
                    InputEvent::PressEnter { .. } | InputEvent::Blur => {
                        surface.show_ticket_number(number, window, surface_cx);
                    }
                    InputEvent::Focus => {}
                });
            })
            .detach();
    }
}

impl WorkspaceSurface {
    fn apply_ticket_number_text(&mut self, number: TicketNumber, cx: &mut Context<Self>) {
        let text = self.order_ticket.input(number).read(cx).value();
        match number.parse(text.as_ref()) {
            Some(ticks) => {
                number.set(&mut self.trading_pnl.order_entry.ticket, ticks);
                self.order_ticket.invalid[number.index()] = false;
            }
            // An emptied field is mid-edit, not an error yet.
            None => self.order_ticket.invalid[number.index()] = !text.trim().is_empty(),
        }
        cx.notify();
    }

    fn show_ticket_number(
        &mut self,
        number: TicketNumber,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ticks = number.value(&self.trading_pnl.order_entry.ticket);
        self.order_ticket.invalid[number.index()] = false;
        let input = self.order_ticket.input(number).clone();
        if number.parse(input.read(cx).value().as_ref()) != Some(ticks) {
            input.update(cx, |input, input_cx| {
                input.set_value(ticks.to_string(), window, input_cx);
            });
        }
        cx.notify();
    }

    fn update_ticket(&mut self, change: impl FnOnce(&mut TicketSettings), cx: &mut Context<Self>) {
        change(&mut self.trading_pnl.order_entry.ticket);
        cx.notify();
    }

    fn toggle_ticket_menu(&mut self, menu: TicketMenu, cx: &mut Context<Self>) {
        self.order_ticket.open_menu = (self.order_ticket.open_menu != Some(menu)).then_some(menu);
        cx.notify();
    }

    fn close_ticket_menu(&mut self, cx: &mut Context<Self>) {
        if self.order_ticket.open_menu.take().is_some() {
            cx.notify();
        }
    }

    /// Makes `account_id` the account every order control on this surface trades on.
    pub(super) fn select_trading_account(
        &mut self,
        account_id: aeris_trading::TradingAccountId,
        cx: &mut Context<Self>,
    ) {
        self.trading_pnl.order_entry.selected_account_id = Some(account_id);
        self.order_ticket.open_menu = None;
        cx.notify();
    }
}

/// Which venue holds an account's orders, as the ticket's broker field names it. Live broker
/// accounts are data-only and never offered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TicketBroker {
    Practice,
    Broker,
}

impl TicketBroker {
    const ALL: [Self; 2] = [Self::Practice, Self::Broker];

    pub(super) fn of(account: &aeris_trading::TradingAccount) -> Option<Self> {
        match account.environment {
            aeris_trading::AccountEnvironment::Simulated => Some(Self::Practice),
            aeris_trading::AccountEnvironment::Demo
                if account.venue_id == aeris_trading::venue::BROKER_VENUE_ID =>
            {
                Some(Self::Broker)
            }
            _ => None,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Practice => "Practice",
            Self::Broker => aeris_market_runtime::built_in_provider_presentations()
                .iter()
                .find(|descriptor| {
                    descriptor.broker_venue_id == Some(aeris_trading::venue::BROKER_VENUE_ID)
                })
                .map_or("Broker", |descriptor| descriptor.display_name),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct TradingOrderControlsState<'a> {
    pub(super) app: &'a Entity<WorkspaceSurface>,
    pub(super) frame: Option<&'a aeris_market_data::OrderBookFrame>,
    pub(super) product: Option<&'a InstallProviderInstrument>,
    pub(super) store: &'a trading_store::TradingStore,
    pub(super) trading: &'a super::TradingPaneState,
    pub(super) quantity: &'a super::order_quantity::OrderQuantityControls,
    pub(super) controls: &'a OrderTicketControls,
    pub(super) theme: &'a AerisTheme,
}

/// What the ticket shows and may do for the selected account and instrument.
struct TicketView<'a> {
    account: Option<&'a aeris_trading::TradingAccount>,
    broker: Option<TicketBroker>,
    position: Option<&'a aeris_trading_runtime::PositionPnl>,
    working_orders: usize,
    /// Orders may be placed: an unlocked account and instrument.
    ready: bool,
}

pub(super) fn ticket_scope<'a>(
    store: &'a trading_store::TradingStore,
    selected: Option<&aeris_trading::TradingAccountId>,
    instrument: Option<&aeris_instruments::InstrumentId>,
) -> (
    Option<&'a aeris_trading::TradingAccount>,
    Option<&'a aeris_trading_runtime::PositionPnl>,
    usize,
) {
    let Some(set) = store.current() else {
        return (None, None, 0);
    };
    let account = selected.and_then(|id| set.accounts.iter().find(|account| &account.id == id));
    let Some((account, instrument)) = account.zip(instrument) else {
        return (account, None, 0);
    };
    let position = set.position_pnl.iter().find(|position| {
        position.position.account_id == account.id && &position.position.instrument_id == instrument
    });
    let working_orders = store
        .pane_view(&account.id, instrument)
        .orders()
        .filter(|order| order.status.is_open())
        .count();
    (Some(account), position, working_orders)
}

impl<'a> TicketView<'a> {
    fn new(state: &TradingOrderControlsState<'a>) -> Self {
        let trading = state.trading;
        let set = state.store.current();
        let instrument_id = state
            .product
            .map(|product| product.instrument_id.as_str())
            .or_else(|| state.frame.map(|frame| frame.instrument_id.as_str()));
        let instrument = instrument_id
            .and_then(|id| aeris_instruments::InstrumentId::try_new(id.to_string()).ok());
        let (account, position, working_orders) = ticket_scope(
            state.store,
            trading.order_entry.selected_account_id.as_ref(),
            instrument.as_ref(),
        );
        Self {
            account,
            broker: account.and_then(TicketBroker::of),
            position,
            working_orders,
            ready: account.is_some()
                && super::selected_account_lock_reason(super::PaneLockView {
                    selected_account: trading.order_entry.selected_account_id.as_ref(),
                    locks: set.map_or(&[], |set| set.risk_locks.as_slice()),
                })
                .is_none()
                && instrument.is_some(),
        }
    }

    fn account_key(&self) -> Option<String> {
        self.account.map(|account| account.id.as_str().to_string())
    }

    fn net_quantity(&self) -> Option<aeris_trading::FixedPoint> {
        self.position.map(|position| position.position.net_quantity)
    }
}

pub(super) fn trading_order_controls(
    state: &TradingOrderControlsState<'_>,
) -> impl IntoElement + use<> {
    let view = TicketView::new(state);
    let theme = state.theme;
    let colors = theme.colors;
    div()
        .relative()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(GAP))
        .p_2()
        .text_xs()
        .bg(gpui_color(colors.surface))
        .text_color(gpui_color(colors.text_default))
        .child(symbol_field(state))
        .child(position_stats(state, &view))
        .child(quantity_field(state))
        .child(broker_field(state, &view))
        .child(account_field(state, &view))
        .child(offset_field(state))
        .child(order_grid(state, &view))
        .child(div().my_1().h_px().bg(gpui_color(colors.border)))
        .child(bracket_header(state))
        .child(bracket_fields(state, &view))
        .child(
            div()
                .mx(px(2.0))
                .mt(px(2.0))
                .text_size(px(11.0))
                .text_color(gpui_color(colors.text_secondary))
                .child(ticket_status(view.net_quantity(), view.working_orders)),
        )
        .children(status_rows(
            state.trading.feedback.as_ref(),
            state
                .store
                .current()
                .and_then(|set| set.market_observation_error.as_ref())
                .map(|error| format!("Trading market update failed: {error}"))
                .as_deref(),
            theme,
        ))
}

fn radius(token: RadiusToken) -> Pixels {
    px(f32::from(token.logical_pixels()))
}

/// The frame every ticket field and statistic shares.
fn field_frame(theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    div()
        .h(px(FIELD_HEIGHT))
        .flex_none()
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .border(platform_border_width(theme))
        .border_color(gpui_color(field_border(theme)))
        .rounded(radius(RadiusToken::Compact))
        .bg(gpui_color(colors.surface_secondary))
}

/// A field inside the switched-off bracket section: no edge, the disabled fill, muted text.
fn locked_field(theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    field_frame(theme)
        .border_color(gpui::transparent_black())
        .bg(gpui_color(colors.disabled_bg))
        .text_color(gpui_color(colors.text_muted))
}

fn field_label(label: &'static str, color: ThemeColor) -> Div {
    div().flex_none().text_color(gpui_color(color)).child(label)
}

fn field_value(value: impl Into<SharedString>) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .justify_end()
        .child(div().truncate().child(value.into()))
}

/// A ticket dropdown: a secondary trigger button with its label on the left and its value
/// against the caret.
fn select_trigger(
    id: &'static str,
    label: Option<&'static str>,
    value: impl Into<SharedString>,
    theme: &AerisTheme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    let colors = theme.colors;
    let value = value.into();
    let button = Button::new(id, theme)
        .variant(ButtonVariant::Secondary)
        .trigger()
        .full_width()
        .text_xs()
        .caret(chevron(colors.icon))
        .aria_label(label.map_or_else(|| value.clone(), |label| format!("{label}: {value}").into()))
        .on_click(on_click);
    match label {
        Some(label) => button
            .leading(field_label(label, colors.text_secondary))
            .child(field_value(value)),
        None => button.child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .child(value),
        ),
    }
}

fn chevron(color: ThemeColor) -> Icon {
    header_icon(HugeIcon::ChevronDown)
        .with_size(px(12.0))
        .color(gpui_color(color))
}

/// A select field: its trigger plus, while open, its menu stretched under it.
fn ticket_select(
    app: &Entity<WorkspaceSurface>,
    id: &'static str,
    trigger: Button,
    menu: Option<MenuPanel>,
) -> MenuAnchor {
    let dismiss = app.clone();
    MenuAnchor::new(id, trigger)
        .full_width()
        .menu(menu)
        .on_dismiss(move |_, cx| {
            dismiss.update(cx, WorkspaceSurface::close_ticket_menu);
        })
}

fn select_menu(id: &'static str, theme: &AerisTheme) -> MenuPanel {
    MenuPanel::new(
        id,
        MenuPlacement::Anchored {
            side: MenuSide::Below,
            align: MenuAlign::Stretch,
        },
        theme,
    )
    .max_height(px(SELECT_MENU_MAX_HEIGHT))
    .animate_from(PopupAnimationOrigin::TOP_LEFT)
}

fn menu_toggle(
    app: &Entity<WorkspaceSurface>,
    menu: TicketMenu,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let app = app.clone();
    move |_, _, cx| {
        app.update(cx, |surface, surface_cx| {
            surface.toggle_ticket_menu(menu, surface_cx);
        });
    }
}

fn symbol_field(state: &TradingOrderControlsState<'_>) -> impl IntoElement + use<> {
    let symbol = state.product.map_or_else(
        || "Choose a symbol".to_string(),
        |product| product.display_symbol.clone(),
    );
    let open = state.app.clone();
    select_trigger(
        "ticket_symbol",
        None,
        symbol,
        state.theme,
        move |_, window, cx| {
            open.update(cx, |surface, surface_cx| {
                surface.open_chrome_overlay(ChromeOverlay::Instrument, window, surface_cx);
            });
        },
    )
}

fn position_stats(state: &TradingOrderControlsState<'_>, view: &TicketView<'_>) -> Div {
    let theme = state.theme;
    let colors = theme.colors;
    let currency = view
        .account
        .map_or("USD", |account| account.currency.as_str());
    let zero_money = view
        .account
        .and_then(|account| aeris_trading::FixedPoint::try_new(0, account.currency_scale).ok());
    let net = view.net_quantity();
    let tint = match net.map(|net| net.units().signum()) {
        Some(1) => Some((colors.buy_subtle, colors.buy_subtle_foreground)),
        Some(-1) => Some((colors.sell_subtle, colors.sell_subtle_foreground)),
        _ => None,
    };
    // A long or short position tints its row with the side's subtle fill.
    let (label_color, value_color) = tint
        .map_or((colors.text_secondary, colors.text_default), |(_, text)| {
            (text, text)
        });
    let open_qty = field_frame(theme)
        .justify_between()
        .when_some(tint, |row, (fill, _)| {
            row.border_color(gpui::transparent_black())
                .bg(gpui_color(fill))
        })
        .child(field_label("Open Qty", label_color))
        .child(
            div()
                .flex_none()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .text_color(gpui_color(value_color))
                .child(net.map_or_else(|| "0".to_string(), format_quantity)),
        );
    // A broker account whose venue is unreachable has no known open P/L.
    let open_pnl_known = state
        .store
        .current()
        .into_iter()
        .flat_map(|set| &set.account_pnl)
        .find(|pnl| {
            view.account
                .is_some_and(|account| account.id == pnl.account_id)
        })
        .is_none_or(|pnl| pnl.unrealized.is_some());
    let open_pnl = open_pnl_known
        .then(|| {
            view.position
                .map(|position| position.position.unrealized_pnl)
                .or(zero_money)
        })
        .flatten();
    let daily_pnl = view.account.and_then(|account| {
        state
            .store
            .current()?
            .risk_meters
            .iter()
            .find(|meter| meter.account_id == account.id && meter.enabled)
            .map(|meter| meter.current_realized_pnl)
    });
    div()
        .flex()
        .flex_col()
        .gap(px(GAP))
        .child(open_qty)
        .child(money_stat("Open P/L", open_pnl, currency, theme))
        .child(money_stat("Daily P/L", daily_pnl, currency, theme))
}

fn money_stat(
    label: &'static str,
    value: Option<aeris_trading::FixedPoint>,
    currency: &str,
    theme: &AerisTheme,
) -> Div {
    let colors = theme.colors;
    let color = match value.map(|value| value.units().signum()) {
        Some(1) => colors.text_positive,
        Some(-1) => colors.text_negative,
        _ => colors.text_muted,
    };
    field_frame(theme)
        .justify_between()
        .child(field_label(label, colors.text_secondary))
        .child(
            div()
                .flex_none()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .text_color(gpui_color(color))
                .child(
                    value.map_or_else(|| "—".to_string(), |value| format_money(currency, value)),
                ),
        )
}

fn quantity_field(state: &TradingOrderControlsState<'_>) -> impl IntoElement + use<> {
    let theme = state.theme;
    let colors = theme.colors;
    let quantity = state.trading.order_entry.quantity;
    let step = |id: &'static str, plus: bool| {
        let app = state.app.clone();
        step_button(id, plus, theme).on_click(move |_, window, cx| {
            app.update(cx, |surface, surface_cx| {
                let next = if plus {
                    quantity.saturating_add(1)
                } else {
                    quantity.saturating_sub(1)
                };
                surface.set_order_quantity(next, window, surface_cx);
            });
        })
    };
    field_frame(theme)
        .when(state.quantity.ticket_invalid(), |field| {
            field.border_color(gpui_color(colors.danger))
        })
        .child(field_label("Qty", colors.text_secondary))
        .child(super::order_quantity::ticket_quantity_field(state.quantity))
        .child(
            div()
                .flex_none()
                .flex()
                .gap(px(2.0))
                .mr(px(-4.0))
                .child(step("ticket_quantity_decrement", false))
                .child(step("ticket_quantity_increment", true)),
        )
}

/// A ghost button sized to sit inside the quantity field; its glyph is smaller than a
/// button icon, so it is a child rather than the button's icon.
fn step_button(id: &'static str, plus: bool, theme: &AerisTheme) -> Button {
    Button::new(id, theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Sm)
        .aria_label(if plus {
            "Increase quantity"
        } else {
            "Decrease quantity"
        })
        .size(px(STEP_BUTTON_SIZE))
        .p_0()
        .justify_center()
        .child(
            header_icon(if plus { HugeIcon::Add } else { HugeIcon::Minus })
                .with_size(px(12.0))
                .color(gpui_color(theme.colors.icon)),
        )
}

fn broker_field(state: &TradingOrderControlsState<'_>, view: &TicketView<'_>) -> MenuAnchor {
    let theme = state.theme;
    let open = state.controls.open_menu == Some(TicketMenu::Broker);
    let menu = open.then(|| {
        select_menu("ticket_broker_menu", theme).children(TicketBroker::ALL.map(|broker| {
            let account = state
                .store
                .current()
                .and_then(|set| first_account(set, broker))
                .map(|account| account.id.clone());
            let choose = state.app.clone();
            MenuRow::compact(
                ("ticket_broker_option", broker as usize),
                broker.label(),
                theme,
            )
            .checked(view.broker == Some(broker))
            .disabled(account.is_none())
            .on_click(move |_, _, cx| {
                let Some(account) = account.clone() else {
                    return;
                };
                choose.update(cx, |surface, surface_cx| {
                    let selected_broker = surface.lifecycle.trading_store().and_then(|store| {
                        let store = store.read(surface_cx);
                        let id = surface
                            .trading_pnl
                            .order_entry
                            .selected_account_id
                            .as_ref()?;
                        store
                            .current()?
                            .accounts
                            .iter()
                            .find(|account| &account.id == id)
                            .and_then(TicketBroker::of)
                    });
                    if selected_broker == Some(broker) {
                        surface.close_ticket_menu(surface_cx);
                    } else {
                        surface.select_trading_account(account, surface_cx);
                    }
                });
            })
        }))
    });
    let value = view.broker.map_or("Choose", TicketBroker::label);
    ticket_select(
        state.app,
        "ticket_broker_field",
        select_trigger(
            "ticket_broker",
            Some("Broker"),
            value,
            theme,
            menu_toggle(state.app, TicketMenu::Broker),
        ),
        menu,
    )
}

/// The account a broker choice switches to: a connected broker account before one whose
/// venue is unreachable.
fn first_account(
    set: &aeris_trading_runtime::WorkingSet,
    broker: TicketBroker,
) -> Option<&aeris_trading::TradingAccount> {
    let mut accounts = set
        .accounts
        .iter()
        .filter(|account| TicketBroker::of(account) == Some(broker));
    let first = accounts.clone().next();
    accounts
        .find(|account| {
            broker == TicketBroker::Practice || set.connected_broker_accounts.contains(&account.id)
        })
        .or(first)
}

fn account_field(state: &TradingOrderControlsState<'_>, view: &TicketView<'_>) -> MenuAnchor {
    let theme = state.theme;
    let open = state.controls.open_menu == Some(TicketMenu::Account);
    let menu = open.then(|| {
        let accounts = state
            .store
            .current()
            .into_iter()
            .flat_map(|set| &set.accounts)
            .filter(|account| view.broker.is_some() && TicketBroker::of(account) == view.broker)
            .enumerate();
        select_menu("ticket_account_menu", theme).children(accounts.map(|(index, account)| {
            let choose = state.app.clone();
            let account_id = account.id.clone();
            let mut row = MenuRow::compact(
                ("ticket_account_option", index),
                account.display_name.clone(),
                theme,
            )
            .checked(
                view.account
                    .is_some_and(|selected| selected.id == account.id),
            );
            if TicketBroker::of(account) == Some(TicketBroker::Broker)
                && !state
                    .store
                    .current()
                    .is_some_and(|set| set.connected_broker_accounts.contains(&account.id))
            {
                row = row.detail("Not connected");
            }
            row.on_click(move |_, _, cx| {
                choose.update(cx, |surface, surface_cx| {
                    surface.select_trading_account(account_id.clone(), surface_cx);
                });
            })
        }))
    });
    let value = view.account.map_or_else(
        || "Choose".to_string(),
        |account| account.display_name.clone(),
    );
    ticket_select(
        state.app,
        "ticket_account_field",
        select_trigger(
            "ticket_account",
            Some("Account"),
            value,
            theme,
            menu_toggle(state.app, TicketMenu::Account),
        ),
        menu,
    )
}

/// A typed tick distance: the editable field while `enabled`, else its value, muted.
fn ticks_value(
    state: &TradingOrderControlsState<'_>,
    number: TicketNumber,
    enabled: bool,
) -> AnyElement {
    if enabled {
        Input::new(state.controls.input(number))
            .appearance(false)
            .bordered(false)
            .flex_1()
            .into_any_element()
    } else {
        field_value(number.value(&state.trading.order_entry.ticket).to_string())
            .text_color(gpui_color(state.theme.colors.text_muted))
            .into_any_element()
    }
}

fn offset_field(state: &TradingOrderControlsState<'_>) -> Div {
    let theme = state.theme;
    let enabled = state.trading.order_entry.ticket.offset_enabled;
    let toggle = state.app.clone();
    field_frame(theme)
        .when(state.controls.invalid(TicketNumber::Offset), |field| {
            field.border_color(gpui_color(theme.colors.danger))
        })
        .child(
            CheckboxRow::new("ticket_offset_toggle", "Ticks diff", enabled, theme)
                .compact()
                .on_change(move |on, _, _, cx| {
                    toggle.update(cx, |surface, surface_cx| {
                        surface.update_ticket(|ticket| ticket.offset_enabled = on, surface_cx);
                    });
                }),
        )
        .child(ticks_value(state, TicketNumber::Offset, enabled))
}

/// The order a ticket button sends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TicketAction {
    /// At the market.
    Market,
    /// A limit order joining the touch: the bid for a buy, the ask for a sell.
    Touch,
    /// A limit order `offset` ticks behind the touch.
    Limit,
    /// A stop `offset` ticks beyond the touch.
    Stop,
    /// A stop limit whose limit is its stop price.
    StopLimit,
}

impl TicketAction {
    const ROWS: [Self; 5] = [
        Self::Market,
        Self::Touch,
        Self::Limit,
        Self::Stop,
        Self::StopLimit,
    ];

    const fn label(self, side: aeris_trading::OrderSide) -> &'static str {
        match (self, side) {
            (Self::Market, _) => "MKT",
            (Self::Touch, aeris_trading::OrderSide::Buy) => "BID",
            (Self::Touch, aeris_trading::OrderSide::Sell) => "ASK",
            (Self::Limit, _) => "LMT",
            (Self::Stop, _) => "STP",
            (Self::StopLimit, _) => "STP LMT",
        }
    }

    const fn order_type(self) -> aeris_trading::OrderType {
        match self {
            Self::Market => aeris_trading::OrderType::Market,
            Self::Touch | Self::Limit => aeris_trading::OrderType::Limit,
            Self::Stop => aeris_trading::OrderType::Stop,
            Self::StopLimit => aeris_trading::OrderType::StopLimit,
        }
    }

    const fn variant(self, side: aeris_trading::OrderSide) -> ButtonVariant {
        match (self, side) {
            (Self::Market, side) => trade_variant(side),
            (_, aeris_trading::OrderSide::Buy) => ButtonVariant::BuySubtle,
            (_, aeris_trading::OrderSide::Sell) => ButtonVariant::SellSubtle,
        }
    }
}

/// The exact price a ticket action rests at, on the book's price grid. A limit rests behind
/// the touch (below the bid for a buy) and a stop beyond it (above the ask for a buy), each
/// `offset_ticks` away; a stop at the touch would trigger at once, so it needs an offset.
fn ticket_price(
    action: TicketAction,
    side: aeris_trading::OrderSide,
    frame: &aeris_market_data::OrderBookFrame,
    offset_ticks: u32,
) -> Result<Option<i64>, &'static str> {
    use aeris_trading::OrderSide::{Buy, Sell};
    let bid = frame.best_bid.as_ref().map(|level| level.price);
    let ask = frame.best_ask.as_ref().map(|level| level.price);
    let (touch, below) = match (action, side) {
        (TicketAction::Market, _) => return Ok(None),
        (TicketAction::Touch | TicketAction::Limit, Buy)
        | (TicketAction::Stop | TicketAction::StopLimit, Sell) => (bid, true),
        (TicketAction::Touch | TicketAction::Limit, Sell)
        | (TicketAction::Stop | TicketAction::StopLimit, Buy) => (ask, false),
    };
    let touch = touch.ok_or("Waiting for a quoted price on this instrument")?;
    let offset = if action == TicketAction::Touch {
        0
    } else {
        offset_ticks
    };
    if offset == 0 && matches!(action, TicketAction::Stop | TicketAction::StopLimit) {
        return Err(STOP_OFFSET_REQUIRED);
    }
    let distance = if offset == 0 {
        0
    } else {
        let tick = frame
            .price_increment
            .filter(|tick| *tick > 0)
            .ok_or("The instrument's price increment is unknown")?;
        i64::from(offset)
            .checked_mul(tick)
            .ok_or("Order price is invalid")?
    };
    if below {
        touch.checked_sub(distance)
    } else {
        touch.checked_add(distance)
    }
    .filter(|price| *price > 0)
    .map(Some)
    .ok_or("Order price is invalid")
}

fn order_grid(state: &TradingOrderControlsState<'_>, view: &TicketView<'_>) -> Div {
    let theme = state.theme;
    let side_label = |label: &'static str| {
        div()
            .flex_1()
            .py(px(2.0))
            .flex()
            .justify_center()
            .text_size(px(11.0))
            .font_weight(platform_font_weight(TypographyRole::Strong))
            .text_color(gpui_color(theme.colors.text_secondary))
            .child(label)
    };
    let row = |left: Button, right: Button| {
        div()
            .flex()
            .gap(px(GAP))
            .child(left.flex_1())
            .child(right.flex_1())
    };
    let mut grid = div().mt_1().flex().flex_col().gap(px(GAP)).child(
        div()
            .flex()
            .gap(px(GAP))
            .child(side_label("BUY"))
            .child(side_label("SELL")),
    );
    for action in TicketAction::ROWS {
        grid = grid.child(row(
            order_button(state, view, action, aeris_trading::OrderSide::Buy),
            order_button(state, view, action, aeris_trading::OrderSide::Sell),
        ));
    }
    grid.child(row(cancel_button(state, view), flat_button(state, view)))
}

fn order_button(
    state: &TradingOrderControlsState<'_>,
    view: &TicketView<'_>,
    action: TicketAction,
    side: aeris_trading::OrderSide,
) -> Button {
    let has_touch = state.frame.is_some_and(|frame| match side {
        aeris_trading::OrderSide::Buy => frame.best_bid.is_some(),
        aeris_trading::OrderSide::Sell => frame.best_ask.is_some(),
    });
    let enabled = view.ready
        && (action == TicketAction::Market || state.frame.is_some())
        && (action != TicketAction::Touch || has_touch);
    let app = state.app.clone();
    let frame = state.frame.cloned();
    let product = state.product.cloned();
    let account = view.account_key();
    let quantity = state.trading.order_entry.quantity;
    let settings = state.trading.order_entry.ticket;
    let side_id = match side {
        aeris_trading::OrderSide::Buy => "ticket_buy",
        aeris_trading::OrderSide::Sell => "ticket_sell",
    };
    Button::new((side_id, action as usize), state.theme)
        .variant(action.variant(side))
        .label(action.label(side))
        .disabled(!enabled)
        .on_click(move |_, _, cx| {
            let Some(product) = product.as_ref() else {
                return;
            };
            let service = app.read(cx).lifecycle.trading().clone();
            let feedback = app.read(cx).lifecycle.trading_feedback().clone();
            let pane = app.read(cx).trading_pane_token.clone();
            let price_units = match (action, frame.as_ref()) {
                (TicketAction::Market, _) => None,
                (_, Some(frame)) => match ticket_price(action, side, frame, settings.offset()) {
                    Ok(price_units) => price_units,
                    Err(reason) => {
                        feedback.record_for_pane(&pane, Err(reason.to_string()));
                        return;
                    }
                },
                _ => return,
            };
            aeris_desktop::trading::dispatch_ticket_order(
                &service,
                &feedback,
                &pane,
                product,
                frame.as_ref(),
                aeris_desktop::trading::TicketOrder {
                    side,
                    order_type: action.order_type(),
                    account_key: account.clone(),
                    quantity,
                    price_units,
                    bracket: settings.bracket(),
                },
                cx,
            );
        })
}

/// Cancels every working order of the selected account; it needs no book.
fn cancel_button(state: &TradingOrderControlsState<'_>, view: &TicketView<'_>) -> Button {
    let app = state.app.clone();
    let account = view.account_key();
    Button::new("ticket_cancel", state.theme)
        .variant(ButtonVariant::Destructive)
        .label("Cancel")
        .aria_label("Cancel working orders")
        .disabled(account.is_none())
        .on_click(move |_, _, cx| {
            let service = app.read(cx).lifecycle.trading().clone();
            let feedback = app.read(cx).lifecycle.trading_feedback().clone();
            let pane = app.read(cx).trading_pane_token.clone();
            aeris_desktop::trading::cancel_simulated_account(
                &service,
                &feedback,
                &pane,
                account.clone(),
                cx,
            );
        })
}

/// Cancels the selected account's orders on this instrument and closes its position.
fn flat_button(state: &TradingOrderControlsState<'_>, view: &TicketView<'_>) -> Button {
    let app = state.app.clone();
    let instrument = state
        .product
        .map(|product| product.instrument_id.clone())
        .or_else(|| state.frame.map(|frame| frame.instrument_id.clone()));
    let account = view.account_key();
    Button::new("ticket_flat", state.theme)
        .variant(ButtonVariant::Warning)
        .label("Flat")
        .aria_label("Flatten this instrument")
        .disabled(account.is_none() || instrument.is_none())
        .on_click(move |_, _, cx| {
            let Some(instrument) = instrument.as_ref() else {
                return;
            };
            let service = app.read(cx).lifecycle.trading().clone();
            let feedback = app.read(cx).lifecycle.trading_feedback().clone();
            let pane = app.read(cx).trading_pane_token.clone();
            aeris_desktop::trading::flatten_simulated_account_for(
                &service,
                &feedback,
                &pane,
                instrument,
                account.clone(),
                cx,
            );
        })
}

fn bracket_header(state: &TradingOrderControlsState<'_>) -> Div {
    let toggle = state.app.clone();
    div()
        .p(px(2.0))
        .flex()
        .items_center()
        .justify_between()
        .font_weight(platform_font_weight(TypographyRole::Emphasis))
        .child("OCO Strategy")
        .child(
            Switch::new(
                "ticket_bracket_switch",
                "OCO Strategy",
                state.trading.order_entry.ticket.bracket_enabled,
                state.theme,
            )
            .on_change(move |on, _, _, cx| {
                toggle.update(cx, |surface, surface_cx| {
                    surface.update_ticket(|ticket| ticket.bracket_enabled = on, surface_cx);
                });
            }),
        )
}

/// Only paired brackets exist: the owner's bracket template always carries one stop and at
/// least one target, so the single-leg modes are listed but cannot be chosen.
const BRACKET_MODES: [(&str, bool); 3] = [("SL/TP", true), ("SL only", false), ("TP only", false)];

fn bracket_fields(state: &TradingOrderControlsState<'_>, view: &TicketView<'_>) -> Div {
    let theme = state.theme;
    let colors = theme.colors;
    let enabled = state.trading.order_entry.ticket.bracket_enabled;
    let mode = if enabled {
        let open = state.controls.open_menu == Some(TicketMenu::BracketMode);
        let menu = open.then(|| {
            select_menu("ticket_bracket_mode_menu", theme).children(
                BRACKET_MODES
                    .iter()
                    .enumerate()
                    .map(|(index, (label, available))| {
                        let choose = state.app.clone();
                        MenuRow::compact(("ticket_bracket_mode_option", index), *label, theme)
                            .checked(index == 0)
                            .disabled(!available)
                            .on_click(move |_, _, cx| {
                                choose.update(cx, WorkspaceSurface::close_ticket_menu);
                            })
                    }),
            )
        });
        ticket_select(
            state.app,
            "ticket_bracket_mode_field",
            select_trigger(
                "ticket_bracket_mode",
                Some("Mode"),
                BRACKET_MODES[0].0,
                theme,
                menu_toggle(state.app, TicketMenu::BracketMode),
            ),
            menu,
        )
        .into_any_element()
    } else {
        locked_field(theme)
            .child(field_label("Mode", colors.text_muted))
            .child(field_value(BRACKET_MODES[0].0))
            .child(chevron(colors.text_muted))
            .into_any_element()
    };
    let leg = |label: &'static str, number: TicketNumber| {
        let field = if enabled {
            field_frame(theme).when(state.controls.invalid(number), |field| {
                field.border_color(gpui_color(colors.danger))
            })
        } else {
            locked_field(theme)
        };
        field
            .child(field_label(
                label,
                if enabled {
                    colors.text_secondary
                } else {
                    colors.text_muted
                },
            ))
            .child(ticks_value(state, number, enabled))
    };
    // Where the bracket's legs are held follows the account: a broker holds them on its
    // servers, while a practice bracket is managed here.
    let server_held = view.broker == Some(TicketBroker::Broker);
    div()
        .flex()
        .flex_col()
        .gap(px(GAP))
        .child(mode)
        .child(leg("SL Ticks", TicketNumber::Stop))
        .child(leg("TP Ticks", TicketNumber::Target))
        .child(
            div()
                .flex()
                .gap_4()
                .px(px(2.0))
                .py_1()
                .text_color(gpui_color(colors.text_secondary))
                .child(held_by("Server", server_held, enabled, theme))
                .child(held_by("Client", !server_held, enabled, theme)),
        )
}

/// One read-only choice of where a bracket is held, drawn as a radio.
fn held_by(label: &'static str, selected: bool, enabled: bool, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    let (ring, dot) = match (enabled, selected) {
        (true, true) => (colors.primary, Some(colors.primary)),
        (false, true) => (colors.border_secondary, Some(colors.text_muted)),
        (_, false) => (colors.border, None),
    };
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .when(!enabled, |row| {
            row.text_color(gpui_color(colors.text_muted))
        })
        .child(
            div()
                .size(px(12.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(radius(RadiusToken::Full))
                .border(platform_border_width(theme))
                .border_color(gpui_color(ring))
                .children(dot.map(|dot| {
                    div()
                        .size(px(6.0))
                        .rounded(radius(RadiusToken::Full))
                        .bg(gpui_color(dot))
                })),
        )
        .child(label)
}

/// The position and order count the ticket's status line names, such as
/// `Short 1 · no working orders`.
pub(super) fn ticket_status(
    net: Option<aeris_trading::FixedPoint>,
    working_orders: usize,
) -> String {
    let side = match net {
        Some(net) if net.units() > 0 => format!("Long {}", format_quantity(net)),
        Some(net) if net.units() < 0 => {
            format!("Short {}", format_quantity(net).trim_start_matches('-'))
        }
        _ => "Flat".to_string(),
    };
    let orders = match working_orders {
        0 => "no working orders".to_string(),
        1 => "1 working order".to_string(),
        count => format!("{count} working orders"),
    };
    format!("{side} · {orders}")
}

/// An exact quantity without trailing fractional zeros.
fn format_quantity(value: aeris_trading::FixedPoint) -> String {
    let scale = u32::from(value.scale());
    let units = value.units();
    let sign = if units < 0 { "-" } else { "" };
    let magnitude = u128::from(units.unsigned_abs());
    let base = 10_u128.pow(scale);
    let whole = magnitude / base;
    let fraction = format!("{:0width$}", magnitude % base, width = scale as usize);
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{fraction}")
    }
}

pub(super) fn format_money(currency: &str, value: aeris_trading::FixedPoint) -> String {
    let scale = usize::from(value.scale());
    let units = value.units();
    let sign = if units < 0 { "-" } else { "" };
    let magnitude = units.unsigned_abs();
    let base = 10_u64.saturating_pow(u32::try_from(scale).unwrap_or(18));
    format!(
        "{sign}{currency} {}.{:0scale$}",
        magnitude / base,
        magnitude % base,
        scale = scale
    )
}

pub(super) const fn trade_variant(side: aeris_trading::OrderSide) -> ButtonVariant {
    match side {
        aeris_trading::OrderSide::Buy => ButtonVariant::Buy,
        aeris_trading::OrderSide::Sell => ButtonVariant::Sell,
    }
}

fn status_rows(
    feedback: Option<&aeris_desktop::trading::TradingCommandFeedback>,
    market_error: Option<&str>,
    theme: &AerisTheme,
) -> Vec<Div> {
    feedback
        .map(|feedback| {
            div()
                .mx(px(2.0))
                .text_size(px(11.0))
                .text_color(gpui_color(if feedback.is_error {
                    theme.colors.danger
                } else {
                    theme.colors.text_muted
                }))
                .child(feedback.message.clone())
        })
        .into_iter()
        .chain(market_error.map(|error| {
            div()
                .mx(px(2.0))
                .text_size(px(11.0))
                .text_color(gpui_color(theme.colors.danger))
                .child(error.to_string())
        }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_ticket_uses_registered_venue_display_name() {
        let descriptor = aeris_market_runtime::built_in_provider_presentations()
            .iter()
            .find(|descriptor| {
                descriptor.broker_venue_id == Some(aeris_trading::venue::BROKER_VENUE_ID)
            })
            .expect("attached broker venue has a presentation");
        assert_eq!(TicketBroker::Broker.label(), descriptor.display_name);
        assert_eq!(TicketBroker::Practice.label(), "Practice");
    }

    fn point(units: i64, scale: u8) -> aeris_trading::FixedPoint {
        aeris_trading::FixedPoint::try_new(units, scale).expect("fixed point")
    }

    fn level(price: i64) -> aeris_market_data::OrderBookColumnLevel {
        aeris_market_data::OrderBookColumnLevel {
            price,
            quantity: Some(1),
            order_count: Some(1),
            price_text: price.to_string(),
            quantity_text: "1".to_string(),
            traded_volume: 0,
            traded_volume_text: "0".to_string(),
            relative_size_bps: 10_000,
        }
    }

    /// A 0.25-tick book quoting 100.00 bid and 100.25 ask at scale 2.
    fn book() -> aeris_market_data::OrderBookFrame {
        aeris_market_data::OrderBookFrame {
            provider_id: "test-provider".to_string(),
            instrument_id: "test:instrument".to_string(),
            entitlement_id: "test-entitlement".to_string(),
            session_generation: 1,
            selection_generation: 1,
            revision: 1,
            source_watermark: 7,
            bbo_source_watermark: 7,
            state: aeris_market_data::OrderBookState::Ready,
            price_scale: 2,
            quantity_scale: 0,
            price_increment: Some(25),
            best_bid: Some(level(10_000)),
            best_ask: Some(level(10_025)),
            traded_volumes: std::collections::BTreeMap::new(),
            trade_source_watermark: 7,
            top_of_book_only: false,
            rows: Vec::new(),
            order_queues: None,
        }
    }

    #[test]
    fn market_buttons_use_the_dedicated_trade_variants() {
        // The variants' token ramps are covered in `native_ui::button`.
        use aeris_trading::OrderSide::{Buy, Sell};
        assert_eq!(TicketAction::Market.variant(Buy), ButtonVariant::Buy);
        assert_eq!(TicketAction::Market.variant(Sell), ButtonVariant::Sell);
        for action in [
            TicketAction::Touch,
            TicketAction::Limit,
            TicketAction::Stop,
            TicketAction::StopLimit,
        ] {
            assert_eq!(action.variant(Buy), ButtonVariant::BuySubtle);
            assert_eq!(action.variant(Sell), ButtonVariant::SellSubtle);
        }
    }

    #[test]
    fn ticket_prices_rest_behind_the_touch_for_limits_and_beyond_it_for_stops() {
        use aeris_trading::OrderSide::{Buy, Sell};
        let frame = book();
        assert_eq!(ticket_price(TicketAction::Market, Buy, &frame, 4), Ok(None));
        // Joining ignores the offset.
        assert_eq!(
            ticket_price(TicketAction::Touch, Buy, &frame, 4),
            Ok(Some(10_000))
        );
        assert_eq!(
            ticket_price(TicketAction::Touch, Sell, &frame, 4),
            Ok(Some(10_025))
        );
        assert_eq!(
            ticket_price(TicketAction::Limit, Buy, &frame, 0),
            Ok(Some(10_000))
        );
        assert_eq!(
            ticket_price(TicketAction::Limit, Buy, &frame, 2),
            Ok(Some(9_950))
        );
        assert_eq!(
            ticket_price(TicketAction::Limit, Sell, &frame, 2),
            Ok(Some(10_075))
        );
        assert_eq!(
            ticket_price(TicketAction::Stop, Buy, &frame, 2),
            Ok(Some(10_075))
        );
        assert_eq!(
            ticket_price(TicketAction::StopLimit, Sell, &frame, 2),
            Ok(Some(9_950))
        );
        assert_eq!(
            ticket_price(TicketAction::Stop, Buy, &frame, 0),
            Err(STOP_OFFSET_REQUIRED)
        );
        // An offset through zero is refused rather than sent as a non-positive price.
        assert!(ticket_price(TicketAction::Limit, Buy, &frame, 400).is_err());
        let mut no_grid = book();
        no_grid.price_increment = None;
        assert!(ticket_price(TicketAction::Limit, Buy, &no_grid, 1).is_err());
        assert_eq!(
            ticket_price(TicketAction::Limit, Buy, &no_grid, 0),
            Ok(Some(10_000))
        );
        let mut no_bid = book();
        no_bid.best_bid = None;
        assert!(ticket_price(TicketAction::Touch, Buy, &no_bid, 0).is_err());
    }

    #[test]
    fn typed_ticks_stay_within_each_fields_range() {
        assert_eq!(TicketNumber::Offset.parse(" 0 "), Some(0));
        assert_eq!(TicketNumber::Stop.parse("0"), None);
        assert_eq!(TicketNumber::Target.parse("16"), Some(16));
        for rejected in ["", "-1", "1.5", "1000", "ten"] {
            assert_eq!(TicketNumber::Stop.parse(rejected), None, "{rejected:?}");
        }
        let mut settings = TicketSettings::default();
        TicketNumber::Offset.set(&mut settings, 3);
        assert_eq!(
            settings.offset(),
            0,
            "an offset that is off sends no distance"
        );
        settings.offset_enabled = true;
        assert_eq!(settings.offset(), 3);
        assert_eq!(settings.bracket(), None);
        settings.bracket_enabled = true;
        assert_eq!(
            settings.bracket(),
            Some(aeris_desktop::trading::TicketBracket {
                stop_ticks: 8,
                target_ticks: 16
            })
        );
    }

    #[test]
    fn the_status_line_names_the_position_and_working_orders() {
        assert_eq!(
            ticket_status(Some(point(-1, 0)), 0),
            "Short 1 · no working orders"
        );
        assert_eq!(
            ticket_status(Some(point(2, 0)), 1),
            "Long 2 · 1 working order"
        );
        assert_eq!(ticket_status(None, 3), "Flat · 3 working orders");
        assert_eq!(
            ticket_status(Some(point(0, 0)), 0),
            "Flat · no working orders"
        );
    }

    #[test]
    fn trading_projection_baseline_ticket_account_and_status_labels() {
        let mut account = aeris_trading::TradingAccount {
            id: aeris_trading::TradingAccountId::try_new("fixture-account").expect("account"),
            display_name: "Fixture practice".to_string(),
            environment: aeris_trading::AccountEnvironment::Simulated,
            venue_id: "aeris-sim".to_string(),
            broker_ref: None,
            currency: "USD".to_string(),
            currency_scale: 2,
            starting_equity: None,
        };
        assert_eq!(
            TicketBroker::of(&account).map(TicketBroker::label),
            Some("Practice")
        );
        assert_eq!(account.display_name, "Fixture practice");
        assert_eq!(ticket_status(None, 2), "Flat · 2 working orders");
        account.environment = aeris_trading::AccountEnvironment::Demo;
        account.venue_id = aeris_trading::venue::BROKER_VENUE_ID.to_string();
        assert_eq!(
            TicketBroker::of(&account).map(TicketBroker::label),
            aeris_market_runtime::built_in_provider_presentations()
                .iter()
                .find(|descriptor| descriptor.broker_venue_id == Some(account.venue_id.as_str()))
                .map(|descriptor| descriptor.display_name)
        );
    }

    #[test]
    fn quantities_and_money_keep_their_exact_scale() {
        assert_eq!(format_quantity(point(-1, 0)), "-1");
        assert_eq!(format_quantity(point(1_500, 3)), "1.5");
        assert_eq!(format_quantity(point(-5, 8)), "-0.00000005");
        assert_eq!(format_quantity(point(2_000, 3)), "2");
        assert_eq!(format_money("USD", point(-12_345_678, 2)), "-USD 123456.78");
    }
}
