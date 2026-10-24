#![cfg(test)]

use super::*;

const RITHMIC: TerminalProvider = TerminalProvider::Rithmic;
const HYPERLIQUID: TerminalProvider = TerminalProvider::Hyperliquid;

fn search_result(index: usize) -> InstrumentMenuSelection {
    InstrumentMenuSelection::SearchResult {
        provider: RITHMIC,
        index,
    }
}

fn expanded_contract(index: usize) -> InstrumentMenuSelection {
    InstrumentMenuSelection::ExpandedContract {
        provider: RITHMIC,
        index,
    }
}

fn result(symbol: &str) -> ProviderInstrumentSummary {
    ProviderInstrumentSummary {
        kind: ProviderInstrumentKind::Instrument,
        symbol: symbol.to_string(),
        display_symbol: symbol.to_string(),
        exchange: "CME".to_string(),
        name: Some(format!("{symbol} future")),
        product_code: Some("ES".to_string()),
        instrument_type: Some("FUTURE".to_string()),
        expiration_date: None,
        asset_class: None,
    }
}

fn market(symbol: &str) -> ProviderInstrumentSummary {
    ProviderInstrumentSummary {
        symbol: symbol.to_string(),
        display_symbol: format!("{symbol}-USDC"),
        exchange: "HL".to_string(),
        asset_class: Some("perpetual".to_string()),
        ..ProviderInstrumentSummary::default()
    }
}

fn product(code: &str, name: Option<&str>) -> ProviderInstrumentSummary {
    ProviderInstrumentSummary {
        kind: ProviderInstrumentKind::Product,
        symbol: code.to_string(),
        display_symbol: code.to_string(),
        exchange: "CME".to_string(),
        name: name.map(str::to_string),
        product_code: Some(code.to_string()),
        asset_class: Some("futures".to_string()),
        ..ProviderInstrumentSummary::default()
    }
}

fn contract(symbol: &str, code: &str, expiration: &str) -> ProviderInstrumentSummary {
    ProviderInstrumentSummary {
        kind: ProviderInstrumentKind::Instrument,
        symbol: symbol.to_string(),
        display_symbol: symbol.to_string(),
        exchange: "CME".to_string(),
        name: Some(format!("{code} contract")),
        product_code: Some(code.to_string()),
        instrument_type: Some("Future".to_string()),
        expiration_date: Some(expiration.to_string()),
        asset_class: None,
    }
}

fn key(code: &str) -> ProductKey {
    ProductKey {
        provider: RITHMIC,
        code: code.to_string(),
        exchange: "CME".to_string(),
    }
}

fn single() -> SymbolBrowser {
    SymbolBrowser::new(SymbolSearchScope::Provider("rithmic"))
}

fn begin(browser: &mut SymbolBrowser, query: &str) -> SymbolSearchRequest {
    let mut requests = browser.begin_search(query).expect("query validates");
    assert_eq!(requests.len(), 1, "a provider scope searches one lane");
    requests.remove(0)
}

fn labels(browser: &SymbolBrowser) -> Vec<(String, bool)> {
    browser
        .menu_entries()
        .into_iter()
        .map(|entry| (entry.label, entry.nested))
        .collect()
}

fn listed_browser() -> SymbolBrowser {
    let mut browser = single();
    let listing = begin(&mut browser, "");
    assert_eq!(listing.lane, SymbolSearchLane::Query);
    assert!(browser.apply_results(
        RITHMIC,
        listing.request_id,
        vec![
            product("ES", Some("E-mini S&P 500")),
            product("NQ", Some("E-mini Nasdaq-100")),
        ],
    ));
    browser
}

fn all_scope() -> (SymbolBrowser, SymbolSearchRequest, SymbolSearchRequest) {
    let mut browser = SymbolBrowser::new(SymbolSearchScope::All);
    let requests = browser.begin_search("BT").expect("query validates");
    let providers: Vec<_> = requests.iter().map(|request| request.provider).collect();
    assert_eq!(providers, [RITHMIC, HYPERLIQUID], "every provider at once");
    let mut requests = requests.into_iter();
    let first = requests.next().expect("first lane");
    let second = requests.next().expect("second lane");
    (browser, first, second)
}

#[test]
fn symbol_browser_fences_stale_searches_and_selections() {
    let mut browser = single();
    let first = begin(&mut browser, " ES ");
    assert_eq!(first.query, "ES");
    let second = begin(&mut browser, "NQ");
    assert!(!browser.apply_results(RITHMIC, first.request_id, vec![result("ESM7")]));
    assert!(!browser.has_results());
    assert!(browser.apply_results(RITHMIC, second.request_id, vec![result("NQM7")]));

    let first_selection = browser
        .select(search_result(0))
        .expect("result can be selected");
    let second_selection = browser
        .select(search_result(0))
        .expect("replacement can be selected");
    assert!(second_selection.generation > first_selection.generation);
    assert_eq!(second_selection.provider, RITHMIC);
    assert!(browser.selected().is_none());
    assert!(!browser.confirm_selection(first_selection.generation));
    assert!(browser.confirm_selection(second_selection.generation));
    assert_eq!(browser.selected(), Some(&second_selection));
}

#[test]
fn resolving_a_secondary_selection_does_not_replace_the_chart_selection() {
    let mut browser = single();
    let search = begin(&mut browser, "ES");
    assert!(browser.apply_results(RITHMIC, search.request_id, vec![result("ESM7")]));
    let chart = browser.select(search_result(0)).expect("chart selection");
    assert!(browser.confirm_selection(chart.generation));

    let watchlist = browser
        .select(search_result(0))
        .expect("watchlist selection");
    assert_eq!(
        browser.resolve_selection(watchlist.generation),
        Some(watchlist)
    );
    assert_eq!(browser.selected(), Some(&chart));
}

#[test]
fn symbol_browser_bounds_queries_results_and_indexes() {
    let mut browser = single();
    assert_eq!(browser.maximum_results(), MAXIMUM_SYMBOL_RESULTS);
    assert!(browser.begin_search("").is_ok(), "an empty query lists");
    assert!(
        browser
            .begin_search(&"x".repeat(MAXIMUM_SYMBOL_QUERY_BYTES + 1))
            .is_err()
    );
    assert!(browser.begin_search("E\nS").is_err());

    let request = begin(&mut browser, "ES");
    assert!(browser.search_pending());
    assert!(!browser.apply_results(
        RITHMIC,
        request.request_id,
        vec![result("ES"); MAXIMUM_SYMBOL_RESULTS + 1],
    ));
    assert!(browser.select(search_result(0)).is_none());
    assert!(
        browser.search_pending(),
        "an oversized reply leaves the search pending"
    );
    assert!(browser.apply_results(
        RITHMIC,
        request.request_id,
        vec![result("ES"); MAXIMUM_SYMBOL_RESULTS],
    ));
}

#[test]
fn rapid_typing_supersedes_without_queueing() {
    let mut browser = single();
    let first = begin(&mut browser, "E");
    let second = begin(&mut browser, "ES");
    let latest = begin(&mut browser, "ESM");
    assert!(latest.request_id > second.request_id && second.request_id > first.request_id);
    assert!(!browser.apply_results(RITHMIC, first.request_id, vec![result("ESU6")]));
    assert!(!browser.apply_results(RITHMIC, second.request_id, vec![result("ESU6")]));
    assert!(browser.search_pending());
    assert!(browser.apply_results(RITHMIC, latest.request_id, vec![result("ESM7")]));
    assert!(browser.lists_query("ESM"));
    assert_eq!(browser.begin_contract_searches(), Vec::new());
}

#[test]
fn changing_scope_fences_searches_but_keeps_the_chart_selection() {
    let mut browser = single();
    let search = begin(&mut browser, "ES");
    assert!(browser.apply_results(RITHMIC, search.request_id, vec![result("ESM7")]));
    let selection = browser.select(search_result(0)).expect("selection");
    assert!(browser.confirm_selection(selection.generation));
    let pending = begin(&mut browser, "NQ");

    assert!(browser.set_scope(SymbolSearchScope::Provider("hyperliquid")));
    assert!(!browser.set_scope(SymbolSearchScope::Provider("hyperliquid")));
    assert!(!browser.covers(RITHMIC));
    assert!(browser.covers(HYPERLIQUID));
    assert_eq!(browser.scoped_provider(), Some(HYPERLIQUID));
    assert!(!browser.apply_results(RITHMIC, pending.request_id, vec![result("NQM7")]));
    assert!(!browser.has_results());
    assert_eq!(browser.selected(), Some(&selection));
    let next = browser.begin_search("BTC").expect("new scope searches");
    assert!(next[0].request_id > pending.request_id);
    assert_eq!(next[0].provider, HYPERLIQUID);
}

#[test]
fn search_and_selection_rejections_are_fenced_separately() {
    let mut browser = single();
    let first_search = begin(&mut browser, "ES");
    assert!(browser.apply_results(RITHMIC, first_search.request_id, vec![result("ESM7")]));
    let first_selection = browser.select(search_result(0)).expect("first selection");
    let second_selection = browser.select(search_result(0)).expect("second selection");
    let second_search = begin(&mut browser, "NQ");

    assert!(!browser.reject_search(
        HYPERLIQUID,
        second_search.request_id,
        SymbolSearchFailure::Failed
    ));
    assert!(browser.reject_search(
        RITHMIC,
        second_search.request_id,
        SymbolSearchFailure::Failed
    ));
    assert_eq!(browser.status(), SymbolBrowserStatus::Failed);
    assert!(!browser.reject_selection(first_selection.generation));
    assert!(browser.confirm_selection(second_selection.generation));
}

#[test]
fn rejected_replacement_preserves_the_confirmed_selection() {
    let mut browser = single();
    let search = begin(&mut browser, "ES");
    assert!(browser.apply_results(
        RITHMIC,
        search.request_id,
        vec![result("ESM7"), result("ESU7")]
    ));
    let current = browser.select(search_result(0)).expect("current selection");
    assert!(browser.confirm_selection(current.generation));
    let replacement = browser
        .select(search_result(1))
        .expect("replacement selection");

    assert!(browser.reject_selection(replacement.generation));
    assert_eq!(browser.selected(), Some(&current));
}

#[test]
fn consuming_completed_search_prevents_reusing_selection_authorization() {
    let mut browser = single();
    let search = begin(&mut browser, "");
    assert!(browser.apply_results(RITHMIC, search.request_id, vec![result("ESM7")]));
    let selection = browser
        .select(search_result(0))
        .expect("catalog result is selectable");
    assert!(browser.confirm_selection(selection.generation));

    assert!(!browser.consume_completed_search(HYPERLIQUID, selection.search_generation));
    assert!(browser.consume_completed_search(RITHMIC, selection.search_generation));
    assert!(!browser.has_results());
    assert!(browser.select(search_result(0)).is_none());
    assert!(!browser.consume_completed_search(RITHMIC, selection.search_generation));
    assert_eq!(browser.selected(), Some(&selection));
}

#[test]
fn a_completed_search_rejects_duplicate_and_stale_results() {
    let mut browser = single();
    let search = begin(&mut browser, "ES");
    assert!(browser.apply_results(
        RITHMIC,
        search.request_id,
        vec![result("ESZ6"), result("ESH7")]
    ));
    assert!(!browser.apply_results(RITHMIC, search.request_id, vec![result("DUPLICATE")]));
    let next = begin(&mut browser, "NQ");
    assert!(!browser.apply_results(RITHMIC, search.request_id, vec![result("STALE")]));
    assert!(browser.apply_results(RITHMIC, next.request_id, vec![result("NQZ6")]));
}

#[test]
fn all_scope_shows_each_provider_section_as_it_answers() {
    let (mut browser, rithmic, hyperliquid) = all_scope();
    assert_eq!(rithmic.request_id, hyperliquid.request_id);
    assert_eq!(rithmic.exchange, None);
    assert_eq!(browser.status(), SymbolBrowserStatus::Searching);
    let sections: Vec<_> = browser
        .menu_entries()
        .into_iter()
        .map(|entry| (entry.label, entry.detail, entry.section))
        .collect();
    assert_eq!(
        sections,
        [
            (
                terminal_provider_display(RITHMIC).to_string(),
                Some("Searching".to_string()),
                true
            ),
            (
                terminal_provider_display(HYPERLIQUID).to_string(),
                Some("Searching".to_string()),
                true
            ),
        ]
    );

    assert!(browser.apply_results(HYPERLIQUID, hyperliquid.request_id, vec![market("BTC")]));
    let entries = browser.menu_entries();
    assert_eq!(entries.len(), 3, "the faster provider's rows show at once");
    assert_eq!(entries[0].detail.as_deref(), Some("Searching"));
    assert!(entries[1].section);
    assert_eq!(entries[1].detail.as_deref(), Some("1 market"));
    assert_eq!(entries[2].label, "BTC-USDC");
    assert_eq!(entries[2].provider, HYPERLIQUID);
    assert_eq!(
        entries[2].action,
        InstrumentMenuAction::Select(InstrumentMenuSelection::SearchResult {
            provider: HYPERLIQUID,
            index: 0
        })
    );

    assert!(browser.apply_results(RITHMIC, rithmic.request_id, Vec::new()));
    assert_eq!(browser.status(), SymbolBrowserStatus::Sections);
    assert_eq!(
        browser.menu_entries()[0].detail.as_deref(),
        Some("No matches")
    );
}

#[test]
fn all_scope_hides_an_unavailable_provider_and_retries_when_ready() {
    let (mut browser, rithmic, hyperliquid) = all_scope();
    assert!(browser.reject_search(
        RITHMIC,
        rithmic.request_id,
        SymbolSearchFailure::Unavailable
    ));
    let entries = browser.menu_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].provider, HYPERLIQUID);

    let retry = browser
        .retry_lane(RITHMIC)
        .expect("a ready catalog answers the latest query");
    assert_eq!(retry.query, "BT");
    assert!(retry.request_id > hyperliquid.request_id);
    assert!(browser.retry_lane(RITHMIC).is_none(), "one retry in flight");
    assert!(browser.retry_lane(HYPERLIQUID).is_none(), "still pending");

    assert!(browser.apply_results(RITHMIC, retry.request_id, vec![result("BTCZ6")]));
    assert!(browser.retry_lane(RITHMIC).is_none(), "already answered");
    assert_eq!(browser.menu_entries()[0].provider, RITHMIC);
}

#[test]
fn a_failed_provider_section_says_so_without_hiding_others() {
    let (mut browser, rithmic, hyperliquid) = all_scope();
    assert!(browser.reject_search(RITHMIC, rithmic.request_id, SymbolSearchFailure::Failed));
    assert!(browser.apply_results(HYPERLIQUID, hyperliquid.request_id, vec![market("BTC")]));
    let entries = browser.menu_entries();
    assert_eq!(
        entries[0].detail.as_deref(),
        Some("Search failed · try again")
    );
    assert_eq!(entries[1].provider, HYPERLIQUID);
}

#[test]
fn exact_symbols_resolve_in_provider_order() {
    let (mut browser, rithmic, hyperliquid) = all_scope();
    assert!(browser.apply_results(HYPERLIQUID, hyperliquid.request_id, vec![market("BTC")]));
    assert_eq!(browser.exact_symbol("btc"), ExactSymbolMatch::Pending);
    assert!(browser.apply_results(RITHMIC, rithmic.request_id, vec![result("ESZ6")]));
    assert_eq!(
        browser.exact_symbol("btc-usdc"),
        ExactSymbolMatch::Found(InstrumentMenuSelection::SearchResult {
            provider: HYPERLIQUID,
            index: 0
        })
    );
    assert_eq!(
        browser.exact_symbol("ESZ6"),
        ExactSymbolMatch::Found(search_result(0))
    );
    assert_eq!(browser.exact_symbol("NQZ6"), ExactSymbolMatch::Missing);
}

#[test]
fn exchanges_are_learned_per_provider_and_bounded() {
    let (mut browser, rithmic, hyperliquid) = all_scope();
    let mut cbot = result("ZBZ6");
    cbot.exchange = "CBOT".to_string();
    let mut invalid = result("BAD");
    invalid.exchange = "CME GLOBEX".to_string();
    assert!(browser.apply_results(
        RITHMIC,
        rithmic.request_id,
        vec![result("ESZ6"), cbot, invalid, result("ESH7")]
    ));
    assert!(browser.apply_results(HYPERLIQUID, hyperliquid.request_id, vec![market("BTC")]));
    let names = |provider| {
        browser
            .exchanges(provider)
            .iter()
            .map(|code| code.as_str().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(RITHMIC), ["CME", "CBOT"]);
    assert_eq!(names(HYPERLIQUID), ["HL"]);

    let mut browser = single();
    let many = (0..MAXIMUM_LEARNED_EXCHANGES + 8)
        .map(|index| {
            let mut contract = result(&format!("X{index}"));
            contract.exchange = format!("X{index}");
            contract
        })
        .collect();
    let search = begin(&mut browser, "X");
    assert!(browser.apply_results(RITHMIC, search.request_id, many));
    assert_eq!(browser.exchanges(RITHMIC).len(), MAXIMUM_LEARNED_EXCHANGES);
}

#[test]
fn an_exchange_scope_sends_its_exchange_with_every_search() {
    let exchange = ExchangeCode::new("CBOT").expect("exchange code");
    let mut browser = SymbolBrowser::new(SymbolSearchScope::Exchange {
        provider: "rithmic",
        exchange,
    });
    let listing = begin(&mut browser, "");
    assert_eq!(listing.exchange, Some(exchange));
    let mut zb = product("ZB", Some("30-Year T-Bond"));
    zb.exchange = "CBOT".to_string();
    assert!(browser.apply_results(RITHMIC, listing.request_id, vec![zb]));
    let contracts = browser
        .toggle_product(&ProductKey {
            provider: RITHMIC,
            code: "ZB".to_string(),
            exchange: "CBOT".to_string(),
        })
        .expect("contract search");
    assert_eq!(contracts.exchange, Some(exchange));
    assert_eq!(contracts.lane, SymbolSearchLane::ProductContracts);
}

#[test]
fn product_listing_shows_collapsed_unselectable_products() {
    let mut browser = listed_browser();
    let entries = browser.menu_entries();
    assert_eq!(entries.len(), 2, "a single provider has no section header");
    assert_eq!(entries[1].label, "NQ");
    assert_eq!(entries[1].description.as_deref(), Some("E-mini Nasdaq-100"));
    assert_eq!(entries[1].detail.as_deref(), Some("futures · CME"));
    assert_eq!(entries[1].expanded, Some(false));
    assert_eq!(
        entries[1].action,
        InstrumentMenuAction::ToggleProduct(key("NQ"))
    );
    assert!(
        browser.select(search_result(1)).is_none(),
        "a product is never sent as a selection"
    );
    assert_eq!(
        browser.status(),
        SymbolBrowserStatus::Listed(SymbolListingSummary::Products(2))
    );
    assert!(browser.lists_query(""));
    assert!(!browser.lists_query("NQ"));
}

#[test]
fn expanding_a_product_searches_and_nests_its_contracts() {
    let mut browser = listed_browser();
    let search = browser
        .toggle_product(&key("NQ"))
        .expect("an idle lane searches the product's contracts");
    assert_eq!(search.query, "NQ");
    assert_eq!(search.lane, SymbolSearchLane::ProductContracts);
    assert_eq!(
        labels(&browser),
        [
            ("ES".to_string(), false),
            ("NQ".to_string(), false),
            ("Loading contracts".to_string(), true),
        ]
    );
    assert_eq!(
        browser.menu_entries()[2].action,
        InstrumentMenuAction::Inert
    );

    assert!(browser.apply_results(
        RITHMIC,
        search.request_id,
        vec![
            contract("NQZ6", "NQ", "20261218"),
            contract("NQH7", "NQ", "20270319"),
        ],
    ));
    let entries = browser.menu_entries();
    assert_eq!(
        entries.len(),
        4,
        "the products stay listed around the contracts"
    );
    assert_eq!(entries[1].expanded, Some(true));
    assert_eq!(entries[2].label, "NQZ6");
    assert!(entries[2].nested);
    assert_eq!(
        entries[2].description.as_deref(),
        Some("E-mini Nasdaq-100 (Dec 2026)")
    );
    assert_eq!(
        entries[3].action,
        InstrumentMenuAction::Select(expanded_contract(1))
    );

    let selection = browser
        .select(expanded_contract(0))
        .expect("an expanded contract is selectable");
    assert_eq!(selection.instrument.symbol, "NQZ6");
    assert_eq!(selection.search_generation, search.request_id);

    assert!(browser.toggle_product(&key("NQ")).is_none());
    assert_eq!(labels(&browser).len(), 2, "collapsing hides the contracts");
    assert!(browser.select(expanded_contract(0)).is_none());
}

#[test]
fn expanding_another_product_replaces_the_pending_contract_search() {
    let mut browser = listed_browser();
    let nq = browser.toggle_product(&key("NQ")).expect("NQ contracts");
    let es = browser
        .toggle_product(&key("ES"))
        .expect("the newer expansion searches at once");
    assert_eq!(es.query, "ES");
    assert!(
        !browser.apply_results(
            RITHMIC,
            nq.request_id,
            vec![contract("NQZ6", "NQ", "202612")]
        ),
        "the replaced contract search's reply is dropped"
    );
    assert_eq!(
        labels(&browser),
        [
            ("ES".to_string(), false),
            ("Loading contracts".to_string(), true),
            ("NQ".to_string(), false),
        ]
    );
    assert!(browser.apply_results(
        RITHMIC,
        es.request_id,
        vec![contract("ESZ6", "ES", "202612")]
    ));
    assert_eq!(labels(&browser)[1], ("ESZ6".to_string(), true));
}

#[test]
fn expanding_during_a_query_search_starts_after_it() {
    let mut browser = listed_browser();
    let listing = begin(&mut browser, "");
    assert!(
        browser.toggle_product(&key("NQ")).is_none(),
        "the listing is refreshing"
    );
    assert_eq!(browser.begin_contract_searches(), Vec::new());
    assert!(browser.apply_results(
        RITHMIC,
        listing.request_id,
        vec![product("NQ", Some("E-mini Nasdaq-100"))],
    ));
    assert!(browser.toggle_product(&key("NQ")).is_some());
}

#[test]
fn a_rejected_contract_search_collapses_its_product() {
    let mut browser = listed_browser();
    let search = browser.toggle_product(&key("NQ")).expect("NQ contracts");
    assert!(browser.reject_search(RITHMIC, search.request_id, SymbolSearchFailure::Failed));
    assert_eq!(labels(&browser).len(), 2);
    assert_eq!(
        browser.status(),
        SymbolBrowserStatus::Listed(SymbolListingSummary::Products(2)),
        "a contract failure leaves the listing intact"
    );
    assert!(
        browser.toggle_product(&key("NQ")).is_some(),
        "expanding retries"
    );
}

#[test]
fn consuming_an_expanded_contract_selection_keeps_the_product_listing() {
    let mut browser = listed_browser();
    let search = browser.toggle_product(&key("NQ")).expect("NQ contracts");
    assert!(browser.apply_results(
        RITHMIC,
        search.request_id,
        vec![contract("NQZ6", "NQ", "202612")]
    ));
    let selection = browser.select(expanded_contract(0)).expect("contract");
    assert!(browser.confirm_selection(selection.generation));
    assert!(browser.consume_completed_search(RITHMIC, selection.search_generation));
    assert_eq!(labels(&browser).len(), 2);
    assert!(browser.lists_query(""));
    assert_eq!(browser.menu_entries()[1].expanded, Some(false));
}

#[test]
fn typed_contracts_group_under_their_product_name() {
    let mut browser = listed_browser();
    let typed = begin(&mut browser, "NQZ6");
    assert!(browser.apply_results(
        RITHMIC,
        typed.request_id,
        vec![
            contract("NQZ6", "NQ", "20261218"),
            contract("MNQZ6", "MNQ", "20261218"),
            contract("NQH7", "NQ", "20270319"),
        ],
    ));
    assert_eq!(
        labels(&browser),
        [
            ("E-mini Nasdaq-100".to_string(), false),
            ("NQZ6".to_string(), true),
            ("NQH7".to_string(), true),
            ("MNQ".to_string(), false),
            ("MNQZ6".to_string(), true),
        ]
    );
    let entries = browser.menu_entries();
    assert_eq!(entries[0].expanded, Some(true));
    assert_eq!(entries[0].detail.as_deref(), Some("futures · CME"));
    assert_eq!(entries[4].description.as_deref(), Some("MNQ contract"));
    assert_eq!(
        entries[2].action,
        InstrumentMenuAction::Select(search_result(2))
    );
    assert_eq!(
        browser.status(),
        SymbolBrowserStatus::Listed(SymbolListingSummary::Contracts {
            contracts: 3,
            products: 2
        })
    );

    assert!(
        browser.toggle_product(&key("NQ")).is_none(),
        "typed groups toggle without a search"
    );
    assert_eq!(labels(&browser).len(), 3);
    assert!(browser.toggle_product(&key("NQ")).is_none());
    assert_eq!(labels(&browser).len(), 5);
    assert!(!browser.search_pending());
}

#[test]
fn instruments_without_a_product_stay_flat() {
    let mut browser = SymbolBrowser::new(SymbolSearchScope::Provider("hyperliquid"));
    let search = browser.begin_search("").expect("listing").remove(0);
    assert!(browser.apply_results(HYPERLIQUID, search.request_id, vec![market("BTC")]));
    let entries = browser.menu_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].label, "BTC-USDC");
    assert_eq!(entries[0].description, None);
    assert_eq!(entries[0].detail.as_deref(), Some("perpetual"));
    assert!(!entries[0].nested);
    assert_eq!(entries[0].expanded, None);
    assert_eq!(
        browser.status(),
        SymbolBrowserStatus::Listed(SymbolListingSummary::Markets(1))
    );
}

#[test]
fn contract_months_parse_only_dated_expirations() {
    assert_eq!(contract_month("20261218").as_deref(), Some("Dec 2026"));
    assert_eq!(contract_month("2027-06-18").as_deref(), Some("Jun 2027"));
    assert_eq!(contract_month("202703").as_deref(), Some("Mar 2027"));
    for invalid in ["", "2027", "202713", "202700", "Dec26", "2026121"] {
        assert_eq!(contract_month(invalid), None, "{invalid}");
    }
}
