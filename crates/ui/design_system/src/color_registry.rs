//! The single ordered registry of color custom properties in `platform.css`.
//!
//! `build.rs` expands it to resolve every token and `lib.rs` expands it to the
//! typed `ThemeColors` projection, so neither side keeps a parallel index list.
//! The build fails when `platform.css` declares a color missing from this list.

macro_rules! platform_color_registry {
    ($expand:ident) => {
        $expand! {
            surface => "surface",
            surface_secondary => "surface-secondary",
            surface_subtle => "surface-subtle",
            surface_raised => "surface-raised",
            surface_overlay => "surface-overlay",
            surface_inverse => "surface-inverse",
            border => "border",
            border_secondary => "border-secondary",
            border_subtle => "border-subtle",
            border_softer => "border-softer",
            border_strong => "border-strong",
            border_inverse => "border-inverse",
            text_primary => "text-primary",
            text_default => "text-default",
            text_secondary => "text-secondary",
            text_muted => "text-muted",
            text_positive => "text-positive",
            text_negative => "text-negative",
            text_danger => "text-danger",
            text_warning => "text-warning",
            text_interactive => "text-interactive",
            text_hover => "text-hover",
            text_active => "text-active",
            hover_bg => "hover-bg",
            active_bg => "active-bg",
            disabled_bg => "disabled-bg",
            icon => "icon",
            icon_active => "icon-active",
            positive => "positive",
            positive_subtle => "positive-subtle",
            negative => "negative",
            negative_subtle => "negative-subtle",
            warning => "warning",
            warning_subtle => "warning-subtle",
            indigo => "indigo",
            indigo_subtle => "indigo-subtle",
            purple => "purple",
            purple_subtle => "purple-subtle",
            primary => "primary",
            primary_hover => "primary-hover",
            primary_active => "primary-active",
            primary_disabled => "primary-disabled",
            primary_disabled_foreground => "primary-disabled-foreground",
            primary_ring => "primary-ring",
            primary_subtle => "primary-subtle",
            primary_foreground => "primary-foreground",
            danger => "danger",
            danger_hover => "danger-hover",
            danger_active => "danger-active",
            danger_disabled => "danger-disabled",
            danger_disabled_foreground => "danger-disabled-foreground",
            danger_ring => "danger-ring",
            danger_foreground => "danger-foreground",
            button_fill => "button-fill",
            button_fill_hover => "button-fill-hover",
            button_fill_active => "button-fill-active",
            button_fill_foreground => "button-fill-foreground",
            button_fill_subtle => "button-fill-subtle",
            buy => "buy",
            buy_hover => "buy-hover",
            buy_active => "buy-active",
            buy_disabled => "buy-disabled",
            buy_disabled_foreground => "buy-disabled-foreground",
            buy_ring => "buy-ring",
            buy_foreground => "buy-foreground",
            sell => "sell",
            sell_hover => "sell-hover",
            sell_active => "sell-active",
            sell_disabled => "sell-disabled",
            sell_disabled_foreground => "sell-disabled-foreground",
            sell_ring => "sell-ring",
            sell_foreground => "sell-foreground",
            /// Order-book bid depth bar. Pair only with `book_bid_text`.
            book_bid_fill => "book-bid-fill",
            /// Order-book bid price text; passes 4.5:1 on `book_bid_fill`.
            book_bid_text => "book-bid-text",
            /// Order-book ask depth bar. Pair only with `book_ask_text`.
            book_ask_fill => "book-ask-fill",
            /// Order-book ask price text; passes 4.5:1 on `book_ask_fill`.
            book_ask_text => "book-ask-text",
            ring => "ring",
            /// Brand focus ring; always the `primary` color.
            ring_primary => "ring-primary",
            /// Chart candles and volume only. Aeris Charts remains authoritative for chart rendering.
            bullish => "bullish",
            /// Chart candles and volume only. Aeris Charts remains authoritative for chart rendering.
            bearish => "bearish",
        }
    };
}
