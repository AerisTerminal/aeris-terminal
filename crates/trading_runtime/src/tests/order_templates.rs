use super::*;

fn key(value: u64) -> std::num::NonZeroU64 {
    std::num::NonZeroU64::new(value).expect("non-zero key")
}

fn price(units: i64) -> FixedPoint {
    FixedPoint::try_new(units, 2).expect("price")
}

fn two_target(key_value: u64, name: &str) -> OrderTemplate {
    OrderTemplate {
        template_id: order_template_id(key(key_value)),
        revision: 1,
        name: name.to_string(),
        quantity: 2,
        stop_offset_ticks: 8,
        targets: vec![
            OrderTemplateTarget {
                offset_ticks: 8,
                quantity: 1,
            },
            OrderTemplateTarget {
                offset_ticks: 16,
                quantity: 1,
            },
        ],
        trailing_stop: Some(TrailingStopRule {
            activation_ticks: 12,
            distance_ticks: 6,
        }),
    }
}

fn published(service: &TradingService) -> Vec<OrderTemplate> {
    service
        .request(Command::WorkingSet)
        .expect("working set")
        .order_templates
}

fn template_entry(client_id: &str, template_id: &str, time: i64) -> PlaceBracket {
    PlaceBracket {
        entry: PlaceOrder {
            quantity: FixedPoint::try_new(2, 0).expect("quantity"),
            ..market_order(client_id, OrderSide::Buy, 1, time)
        },
        template_id: template_id.to_string(),
    }
}

#[test]
fn order_templates_are_bounded_and_never_evicted() {
    let directory = TestDirectory::new("order-template-bound");
    let service = start_service(&directory);
    for index in 1..=u64::try_from(MAXIMUM_ORDER_TEMPLATES).expect("bound") {
        service
            .save_order_template(two_target(index, "Scalp"))
            .expect("within the bound");
    }
    assert_eq!(
        service.save_order_template(two_target(0x100, "One too many")),
        Err(OrderTemplateError::TooManyTemplates)
    );
    let revised = OrderTemplate {
        revision: 2,
        name: "Scalp wide".to_string(),
        stop_offset_ticks: 12,
        ..two_target(1, "Scalp")
    };
    assert_eq!(
        service.save_order_template(revised.clone()),
        Ok(revised.clone())
    );
    assert_eq!(
        service.save_order_template(revised.clone()),
        Err(OrderTemplateError::RevisionMustAdvance { current: 2 })
    );
    assert_eq!(
        service.save_order_template(OrderTemplate {
            quantity: 3,
            revision: 3,
            ..revised.clone()
        }),
        Err(OrderTemplateError::Invalid(
            OrderTemplateInvalid::TargetTotal
        ))
    );
    let templates = published(&service);
    assert_eq!(templates.len(), MAXIMUM_ORDER_TEMPLATES);
    assert!(templates.contains(&revised));
    assert!(
        templates
            .iter()
            .all(|template| template.template_id != order_template_id(key(0x100)))
    );
    // Order templates are kept apart from the internal bracket templates.
    let reserved = BracketStrategyTemplate {
        template_id: order_template_id(key(0x200)),
        revision: 1,
        name: "Reserved".to_string(),
        stop_offset_ticks: 8,
        targets: vec![BracketTarget {
            offset_ticks: 8,
            quantity_percent: 100,
        }],
        trailing_stop: None,
        break_even: None,
        enabled: true,
    };
    assert!(
        service
            .register_strategy_template(reserved)
            .expect_err("reserved identity")
            .contains("reserved for order templates")
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn saved_and_removed_order_templates_survive_a_restart() {
    let directory = TestDirectory::new("order-template-restart");
    let service = start_service(&directory);
    let kept = two_target(0x2a, "Scalp");
    let removed = two_target(0x2b, "Swing");
    service.save_order_template(kept.clone()).expect("saved");
    service.save_order_template(removed.clone()).expect("saved");
    service
        .remove_order_template(removed.template_id.clone())
        .expect("removed");
    assert_eq!(
        service.remove_order_template(removed.template_id.clone()),
        Err(OrderTemplateError::UnknownTemplate)
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");

    let restarted = start_service(&directory);
    assert_eq!(published(&restarted), vec![kept.clone()]);
    assert_eq!(
        restarted.save_order_template(kept.clone()),
        Err(OrderTemplateError::RevisionMustAdvance { current: 1 })
    );
    restarted
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn an_order_template_places_its_bracket_and_removing_it_keeps_the_group() {
    let directory = TestDirectory::new("order-template-place");
    let instrument = instrument();
    let (service, clock, _feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    let template = two_target(0x2a, "Scalp");
    service
        .save_order_template(template.clone())
        .expect("saved");
    let outcome = service
        .place_bracket(template_entry(
            "template-entry",
            &template.template_id,
            clock.wall_unix_nanos(),
        ))
        .expect("placed");
    let PlacementOutcome::Managed(view) = outcome else {
        panic!("a simulated account manages its bracket: {outcome:?}");
    };
    assert_eq!(view.template.name, "Scalp");
    assert_eq!(view.template.stop_offset_ticks, 8);
    assert_eq!(view.template.trailing_stop, template.trailing_stop);
    assert_eq!(
        view.template
            .targets
            .iter()
            .map(|target| (target.offset_ticks, target.quantity_percent))
            .collect::<Vec<_>>(),
        vec![(8, 50), (16, 50)]
    );

    service
        .remove_order_template(template.template_id.clone())
        .expect("removed");
    let set = service.request(Command::WorkingSet).expect("working set");
    assert_eq!(set.order_templates, Vec::<OrderTemplate>::new());
    let group = set
        .order_groups
        .iter()
        .find(|group| group.template == Some((template.template_id.clone(), 1)))
        .expect("the placed group survives its template");
    assert_eq!(group.legs.len(), 3, "a stop and two targets");
    assert!(
        set.managed_brackets
            .iter()
            .any(|bracket| bracket.entry_client_order_id.as_str() == "template-entry")
    );
    assert!(
        service
            .place_bracket(template_entry(
                "after-removal",
                &template.template_id,
                clock.wall_unix_nanos(),
            ))
            .is_err(),
        "a removed template places nothing"
    );
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}

#[test]
fn an_order_template_entry_keeps_its_own_resting_price() {
    let directory = TestDirectory::new("order-template-resting");
    let instrument = instrument();
    let (service, clock, _feed) =
        quoted_fixture(&directory, &instrument, market_feed::route(), 9_975, 10_000);
    let template = two_target(0x2a, "Scalp");
    service
        .save_order_template(template.clone())
        .expect("saved");
    let mut bracket = template_entry(
        "resting-template",
        &template.template_id,
        clock.wall_unix_nanos(),
    );
    bracket.entry.order_type = OrderType::Limit;
    bracket.entry.time_in_force = TimeInForce::GoodTillCancelled;
    bracket.entry.limit_price = Some(price(9_900));
    service.place_bracket(bracket).expect("placed");
    let set = service.request(Command::WorkingSet).expect("working set");
    let entry = set
        .orders
        .iter()
        .find(|order| order.client_order_id.as_str() == "resting-template")
        .expect("entry order");
    assert_eq!(entry.limit_price, Some(price(9_900)));
    assert_eq!(entry.status, OrderStatus::Working);
    service
        .shutdown_after_owner_exit_for_test()
        .expect("shutdown");
}
