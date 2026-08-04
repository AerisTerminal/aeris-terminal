use crate::{
    Continuation, HistoryCapabilities, HistoryPage, HistoryPageRequest, PaginationStyle,
    ProviderHistoryError, model::page_capability,
};

pub(crate) fn validate_page(
    capabilities: &HistoryCapabilities,
    page: &HistoryPage,
    accepted_at_unix_nanos: i64,
) -> Result<(), ProviderHistoryError> {
    capabilities.validate_request(&page.request, accepted_at_unix_nanos)?;
    let (pagination, maximum) = page_capability(capabilities, &page.request)?;
    if page.items.len() > page.request.maximum_items.get() || page.items.len() > maximum {
        return Err(ProviderHistoryError::InvalidPage(
            "item count exceeds request bound",
        ));
    }
    let effective_end = match page.request.continuation {
        Some(Continuation::EndBeforeUnixNanos(cutoff)) => cutoff,
        _ => page.request.range.end_unix_nanos,
    };
    let mut previous = None;
    for item in &page.items {
        item.validate()?;
        if !page
            .request
            .range
            .start_unix_nanos
            .le(&item.event_time_unix_nanos)
            || item.event_time_unix_nanos >= effective_end
        {
            return Err(ProviderHistoryError::InvalidPage(
                "item falls outside requested range",
            ));
        }
        if previous.is_some_and(|value| item.sequence <= value) {
            return Err(ProviderHistoryError::InvalidPage(
                "items are not strictly ordered",
            ));
        }
        previous = Some(item.sequence);
    }
    match (&page.next, pagination) {
        (None, _) | (Some(Continuation::Cursor(_)), PaginationStyle::OpaqueCursor) => Ok(()),
        (Some(Continuation::EndBeforeUnixNanos(end)), PaginationStyle::EndTime)
            if *end > page.request.range.start_unix_nanos
                && *end < page.request.range.end_unix_nanos =>
        {
            Ok(())
        }
        _ => Err(ProviderHistoryError::InvalidContinuation),
    }
}

pub(crate) fn continuation_progresses(request: &HistoryPageRequest, next: &Continuation) -> bool {
    match (request.continuation.as_ref(), next) {
        (
            Some(Continuation::EndBeforeUnixNanos(previous)),
            Continuation::EndBeforeUnixNanos(next),
        ) => next < previous,
        (None, Continuation::EndBeforeUnixNanos(next)) => *next < request.range.end_unix_nanos,
        (_, Continuation::Cursor(_)) => true,
        _ => false,
    }
}
