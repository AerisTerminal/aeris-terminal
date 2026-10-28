#![cfg(test)]

use super::*;

const RITHMIC: ProviderId = ProviderId("rithmic");
const HYPERLIQUID: ProviderId = ProviderId("hyperliquid");

fn search_result(index: usize) -> InstrumentMenuSelection {
    InstrumentMenuSelection::SearchResult {
        provider: RITHMIC,
        index,
    }
}

fn product_contract(index: usize) -> InstrumentMenuSelection {
    InstrumentMenuSelection::ProductContract {
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

fn delayed(mut contract: ProviderInstrumentSummary) -> ProviderInstrumentSummary {
    contract.exchange = "CME-Delayed".to_string();
    contract
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

#[test]
fn provider_set_comes_from_published_descriptors() {
    use aeris_market_runtime::{ConnectMethod, ProviderDescriptor};

    let descriptors = [
        ProviderDescriptor {
            id: "fixture-third",
            display_name: "Fixture Venue",
            logo_key: "",
            connect: ConnectMethod::Public,
            symbol_browser: false,
            autoload_catalog: false,
            offered_intervals: &["1m"],
        },
        aeris_market_runtime::published_provider_descriptors()[0].clone(),
    ];
    assert!(
        !scope_lanes_from_descriptors(SymbolSearchScope::All, &descriptors)
            .iter()
            .any(|lane| lane.provider.id() == "fixture-third")
    );
    assert_eq!(
        scope_lanes_from_descriptors(SymbolSearchScope::Provider("fixture-third"), &descriptors)
            .len(),
        0
    );
    let mut enabled = descriptors;
    enabled[0].symbol_browser = true;
    let lanes = scope_lanes_from_descriptors(SymbolSearchScope::All, &enabled);
    assert_eq!(
        lanes.first().map(|lane| lane.provider.id()),
        Some("fixture-third")
    );
    assert_eq!(
        scope_lanes_from_descriptors(SymbolSearchScope::Provider("fixture-third"), &enabled).len(),
        1
    );
}

fn begin(browser: &mut SymbolBrowser, query: &str) -> SymbolSearchRequest {
    let mut requests = browser.begin_search(query).expect("query validates");
    assert_eq!(requests.len(), 1, "a provider scope searches one lane");
    requests.remove(0)
}

fn labels(browser: &SymbolBrowser) -> Vec<String> {
    browser
        .menu_entries()
        .into_iter()
        .map(|entry| entry.label)
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
    assert!(!browser.search_pending());
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
    assert_eq!(
        browser.status(),
        SymbolBrowserStatus::NoMatches,
        "a failed search lists nothing rather than an error"
    );
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
fn all_scope_lists_each_provider_as_it_answers_without_headers() {
    let (mut browser, rithmic, hyperliquid) = all_scope();
    assert_eq!(rithmic.request_id, hyperliquid.request_id);
    assert_eq!(rithmic.exchange, None);
    assert_eq!(browser.status(), SymbolBrowserStatus::Searching);
    assert_eq!(browser.menu_entries(), Vec::new());

    assert!(browser.apply_results(HYPERLIQUID, hyperliquid.request_id, vec![market("BTC")]));
    let entries = browser.menu_entries();
    assert_eq!(entries.len(), 1, "the faster provider's rows show at once");
    assert_eq!(entries[0].label, "BTC-USDC");
    assert_eq!(entries[0].provider, HYPERLIQUID);
    assert_eq!(
        entries[0].action,
        InstrumentMenuAction::Select(InstrumentMenuSelection::SearchResult {
            provider: HYPERLIQUID,
            index: 0
        })
    );
    assert_eq!(browser.status(), SymbolBrowserStatus::Searching);

    assert!(browser.apply_results(RITHMIC, rithmic.request_id, vec![result("BTCZ6")]));
    assert_eq!(
        labels(&browser),
        ["BTCZ6", "BTC-USDC"],
        "providers keep their fixed order"
    );
    assert_eq!(
        browser.status(),
        SymbolBrowserStatus::Listed(SymbolListingSummary::Markets(2))
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
    assert!(browser.apply_results(HYPERLIQUID, hyperliquid.request_id, vec![market("BTC")]));
    let entries = browser.menu_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].provider, HYPERLIQUID);

    let retry = browser
        .retry_lane(RITHMIC)
        .expect("a ready catalog answers the latest query");
    assert_eq!(retry.query, "BT");
    assert!(retry.request_id > hyperliquid.request_id);
    assert!(browser.retry_lane(RITHMIC).is_none(), "one retry in flight");
    assert!(
        browser.retry_lane(HYPERLIQUID).is_none(),
        "already answered"
    );

    assert!(browser.apply_results(RITHMIC, retry.request_id, vec![result("BTCZ6")]));
    assert!(browser.retry_lane(RITHMIC).is_none(), "already answered");
    assert_eq!(browser.menu_entries()[0].provider, RITHMIC);
}

#[test]
fn a_failed_provider_lists_nothing_without_hiding_others() {
    let (mut browser, rithmic, hyperliquid) = all_scope();
    assert!(browser.reject_search(RITHMIC, rithmic.request_id, SymbolSearchFailure::Failed));
    assert!(browser.apply_results(HYPERLIQUID, hyperliquid.request_id, vec![market("BTC")]));
    let entries = browser.menu_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].provider, HYPERLIQUID);
    assert_eq!(
        browser.status(),
        SymbolBrowserStatus::Listed(SymbolListingSummary::Markets(1))
    );
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
fn a_product_code_resolves_to_its_nearest_expiry_contract() {
    let mut browser = single();
    let search = begin(&mut browser, "NQ");
    let mut alias = contract("NQ", "NQ", "20261218");
    alias.kind = ProviderInstrumentKind::FrontMonthAlias;
    assert!(browser.apply_results(
        RITHMIC,
        search.request_id,
        vec![
            alias,
            contract("NQH7", "NQ", "20270319"),
            contract("NQZ6", "NQ", "20261218"),
            delayed(contract("NQZ6", "NQ", "20261218")),
        ],
    ));
    assert_eq!(
        browser.exact_symbol("nq"),
        ExactSymbolMatch::Found(search_result(1)),
        "the alias is dropped and the real-time front month wins the tie"
    );
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
        .open_product(&ProductKey {
            provider: RITHMIC,
            code: "ZB".to_string(),
            exchange: "CBOT".to_string(),
        })
        .expect("contract search");
    assert_eq!(contracts.exchange, Some(exchange));
    assert_eq!(contracts.lane, SymbolSearchLane::ProductContracts);
}

#[test]
fn product_listing_rows_open_their_product() {
    let mut browser = listed_browser();
    let entries = browser.menu_entries();
    assert_eq!(entries.len(), 2, "no provider header above the products");
    assert_eq!(entries[1].label, "NQ");
    assert_eq!(entries[1].description.as_deref(), Some("E-mini Nasdaq-100"));
    assert_eq!(entries[1].detail.as_deref(), Some("futures · CME"));
    assert_eq!(
        entries[1].action,
        InstrumentMenuAction::OpenProduct(key("NQ"))
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
fn opening_a_product_selects_its_nearest_expiry_contract_once() {
    let mut browser = listed_browser();
    let search = browser
        .open_product(&key("NQ"))
        .expect("an idle lane searches the product's contracts");
    assert_eq!(search.query, "NQ");
    assert_eq!(search.lane, SymbolSearchLane::ProductContracts);
    assert_eq!(labels(&browser), ["ES", "NQ"], "the listing stays as it is");
    assert_eq!(browser.menu_entries()[1].detail.as_deref(), Some("Opening"));
    assert_eq!(browser.take_opened_product(), None, "still listing");

    assert!(browser.apply_results(
        RITHMIC,
        search.request_id,
        vec![
            contract("NQH7", "NQ", "20270319"),
            delayed(contract("NQZ6", "NQ", "20261218")),
            contract("NQZ6", "NQ", "20261218"),
            contract("ESZ6", "ES", "20261218"),
        ],
    ));
    assert_eq!(
        browser.take_opened_product(),
        Some(OpenedProduct::Contract(product_contract(1))),
        "the earliest expiry on the product's own exchange"
    );
    assert_eq!(browser.take_opened_product(), None, "handed out once");
    let selection = browser
        .select(product_contract(1))
        .expect("the opened contract is selectable");
    assert_eq!(selection.instrument.symbol, "NQZ6");
    assert_eq!(selection.instrument.exchange, "CME");
    assert_eq!(selection.search_generation, search.request_id);
    assert_eq!(
        browser.menu_entries()[1].detail.as_deref(),
        Some("futures · CME")
    );
}

#[test]
fn opening_another_product_replaces_the_pending_one() {
    let mut browser = listed_browser();
    let nq = browser.open_product(&key("NQ")).expect("NQ contracts");
    let es = browser
        .open_product(&key("ES"))
        .expect("the newer product searches at once");
    assert_eq!(es.query, "ES");
    assert!(
        !browser.apply_results(
            RITHMIC,
            nq.request_id,
            vec![contract("NQZ6", "NQ", "202612")]
        ),
        "the replaced contract search's reply is dropped"
    );
    assert!(browser.apply_results(
        RITHMIC,
        es.request_id,
        vec![contract("ESZ6", "ES", "202612")]
    ));
    assert_eq!(
        browser.take_opened_product(),
        Some(OpenedProduct::Contract(product_contract(0)))
    );
}

#[test]
fn a_product_without_contracts_is_reported_once() {
    let mut browser = listed_browser();
    let search = browser.open_product(&key("NQ")).expect("NQ contracts");
    assert!(browser.apply_results(
        RITHMIC,
        search.request_id,
        vec![contract("ESZ6", "ES", "202612")]
    ));
    assert_eq!(
        browser.take_opened_product(),
        Some(OpenedProduct::NoContracts(key("NQ")))
    );
    assert_eq!(browser.take_opened_product(), None);
    assert!(
        browser.open_product(&key("NQ")).is_some(),
        "it can open again"
    );
}

#[test]
fn opening_during_a_listing_refresh_opens_nothing() {
    let mut browser = listed_browser();
    let listing = begin(&mut browser, "");
    assert!(
        browser.open_product(&key("NQ")).is_none(),
        "the listing is refreshing"
    );
    assert!(browser.apply_results(
        RITHMIC,
        listing.request_id,
        vec![product("NQ", Some("E-mini Nasdaq-100"))],
    ));
    assert!(browser.open_product(&key("NQ")).is_some());
}

#[test]
fn a_rejected_contract_search_lets_the_product_open_again() {
    let mut browser = listed_browser();
    let search = browser.open_product(&key("NQ")).expect("NQ contracts");
    assert!(browser.reject_search(RITHMIC, search.request_id, SymbolSearchFailure::Failed));
    assert_eq!(browser.take_opened_product(), None);
    assert_eq!(
        browser.status(),
        SymbolBrowserStatus::Listed(SymbolListingSummary::Products(2)),
        "a contract failure leaves the listing intact"
    );
    assert!(
        browser.open_product(&key("NQ")).is_some(),
        "opening retries"
    );
}

#[test]
fn consuming_an_opened_contract_selection_keeps_the_product_listing() {
    let mut browser = listed_browser();
    let search = browser.open_product(&key("NQ")).expect("NQ contracts");
    assert!(browser.apply_results(
        RITHMIC,
        search.request_id,
        vec![contract("NQZ6", "NQ", "202612")]
    ));
    let selection = browser.select(product_contract(0)).expect("contract");
    assert!(browser.confirm_selection(selection.generation));
    assert!(browser.consume_completed_search(RITHMIC, selection.search_generation));
    assert_eq!(labels(&browser).len(), 2);
    assert!(browser.lists_query(""));
    assert!(browser.select(product_contract(0)).is_none());
}

#[test]
fn typed_contracts_list_flat_with_their_month_and_exchange() {
    let mut browser = listed_browser();
    let typed = begin(&mut browser, "NQ");
    let mut alias = contract("NQ", "NQ", "20261218");
    alias.kind = ProviderInstrumentKind::FrontMonthAlias;
    assert!(browser.apply_results(
        RITHMIC,
        typed.request_id,
        vec![
            contract("NQZ6", "NQ", "20261218"),
            contract("NQH7", "NQ", "20270319"),
            alias,
            delayed(contract("NQZ6", "NQ", "20261218")),
            contract("MNQZ6", "MNQ", "20261218"),
        ],
    ));
    assert_eq!(labels(&browser), ["NQZ6", "NQH7", "NQZ6", "MNQZ6"]);
    let entries = browser.menu_entries();
    assert_eq!(
        entries[0].description.as_deref(),
        Some("E-mini Nasdaq-100 (Dec 2026)")
    );
    assert_eq!(entries[0].detail.as_deref(), Some("CME"));
    assert_eq!(entries[2].detail.as_deref(), Some("CME-Delayed"));
    assert_eq!(
        entries[3].description.as_deref(),
        Some("MNQ contract (Dec 2026)"),
        "a product the listing does not name keeps the provider's name"
    );
    assert_eq!(
        entries[2].action,
        InstrumentMenuAction::Select(search_result(2))
    );
    assert_eq!(
        browser.status(),
        SymbolBrowserStatus::Listed(SymbolListingSummary::Contracts(4))
    );
}

#[test]
fn instruments_without_a_product_stay_plain() {
    let mut browser = SymbolBrowser::new(SymbolSearchScope::Provider("hyperliquid"));
    let search = browser.begin_search("").expect("listing").remove(0);
    assert!(browser.apply_results(HYPERLIQUID, search.request_id, vec![market("BTC")]));
    let entries = browser.menu_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].label, "BTC-USDC");
    assert_eq!(entries[0].description, None);
    assert_eq!(entries[0].detail.as_deref(), Some("perpetual"));
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
