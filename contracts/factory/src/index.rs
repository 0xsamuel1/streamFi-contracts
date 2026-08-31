use soroban_sdk::{Address, Env, Vec};

use crate::{
    query,
    storage::{DataKey, StreamPage},
    ttl,
};

const PAGE_SIZE: u32 = query::MAX_PAGE_SIZE;

fn extend_ttl(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, ttl::THRESHOLD, ttl::EXTEND_TO);
}

fn write_page(env: &Env, key: DataKey, page: &Vec<u64>) {
    env.storage().persistent().set(&key, page);
    extend_ttl(env, &key);
}

fn migrate_legacy_index(
    env: &Env,
    legacy_key: &DataKey,
    count_key: &DataKey,
    mut make_page_key: impl FnMut(u32) -> DataKey,
) -> u32 {
    if let Some(count) = env.storage().persistent().get::<_, u32>(count_key) {
        return count;
    }

    let legacy: Option<Vec<u64>> = env.storage().persistent().get(legacy_key);
    let Some(entries) = legacy else {
        return 0;
    };

    let count = entries.len();
    let mut page = Vec::new(env);
    let mut page_index = 0_u32;

    for entry in entries.iter() {
        page.push_back(entry);
        if page.len() == PAGE_SIZE {
            write_page(env, make_page_key(page_index), &page);
            page = Vec::new(env);
            page_index = page_index.saturating_add(1);
        }
    }

    if !page.is_empty() {
        write_page(env, make_page_key(page_index), &page);
    }

    env.storage().persistent().set(count_key, &count);
    extend_ttl(env, count_key);
    env.storage().persistent().remove(legacy_key);

    count
}

fn append_index_entry(
    env: &Env,
    count_key: &DataKey,
    legacy_key: &DataKey,
    mut make_page_key: impl FnMut(u32) -> DataKey,
    entry: u64,
) {
    let count = migrate_legacy_index(env, legacy_key, count_key, &mut make_page_key);
    let page_index = count / PAGE_SIZE;
    let page_key = make_page_key(page_index);

    let mut page: Vec<u64> = env
        .storage()
        .persistent()
        .get(&page_key)
        .unwrap_or(Vec::new(env));
    page.push_back(entry);
    write_page(env, page_key, &page);

    let new_count = count.saturating_add(1);
    env.storage().persistent().set(count_key, &new_count);
    extend_ttl(env, count_key);

    // The page just written already had its TTL extended (write_page). Keep
    // every *other* full page alive too — a page that has filled is never
    // written again, so without this walk it would only be refreshed when a
    // query happens to land in its range.
    extend_page_ttls(env, &mut make_page_key, new_count);
}

/// Walk every populated page of a paginated index and extend its persistent
/// TTL.
///
/// The index only extends a page's TTL when that specific page is written
/// (`write_page` from [`append_index_entry`]) or read (`collect_page` in the
/// window [`read_index`] touches). Once a page fills it is never written
/// again, and it is only read when a query lands in its range. A UI that
/// browses "most recent first" only touches the newest page, so the oldest
/// full pages are neither written nor read and eventually archive. After an
/// older page archives, `streams_by_sender` / `streams_by_recipient` silently
/// return fewer IDs than `stream_count_by_sender` /
/// `stream_count_by_recipient` report: the archived page's `get` yields an
/// empty `Vec`, which the `page_offset >= page_len` branch in [`read_index`]
/// then skips straight past.
///
/// Calling this from every read/append keeps the whole index alive at a cost
/// linear in the number of pages for that sender/recipient. `has` gating is
/// defensive: if a page is already archived (or otherwise missing) `extend_ttl`
/// could not restore it, so it is skipped rather than touched on every call.
fn extend_page_ttls(env: &Env, make_page_key: &mut impl FnMut(u32) -> DataKey, count: u32) {
    if count == 0 {
        return;
    }
    // `count` is the number of entries; the final entry lives on page
    // (`count` - 1) / PAGE_SIZE. `saturating_sub` keeps the divide well-formed
    // (the `count == 0` case is already returned above).
    let last_page = count.saturating_sub(1) / PAGE_SIZE;
    for page_index in 0..=last_page {
        let key = make_page_key(page_index);
        if env.storage().persistent().has(&key) {
            extend_ttl(env, &key);
        }
    }
}

fn collect_page(
    env: &Env,
    make_page_key: &mut impl FnMut(u32) -> DataKey,
    page_index: u32,
) -> Vec<u64> {
    let key = make_page_key(page_index);
    if env.storage().persistent().has(&key) {
        extend_ttl(env, &key);
    }
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or(Vec::new(env))
}

fn read_index(
    env: &Env,
    count_key: &DataKey,
    legacy_key: &DataKey,
    mut make_page_key: impl FnMut(u32) -> DataKey,
    offset: u32,
    limit: u32,
) -> Vec<u64> {
    if let Some(count) = env.storage().persistent().get::<_, u32>(count_key) {
        extend_ttl(env, count_key);
        // Refresh every populated page on ANY read, not just the page(s) this
        // window touches. A UI that reads only "most recent first" would
        // otherwise leave page 0 (never written again once full) to archive,
        // silently truncating results relative to the count (see
        // extend_page_ttls).
        extend_page_ttls(env, &mut make_page_key, count);
        if offset >= count {
            return Vec::new(env);
        }

        // `limit` is silently capped at PAGE_SIZE (== query::MAX_PAGE_SIZE) so a
        // caller can never force an unbounded read. Callers can't tell a capped
        // page from a sender's whole (smaller) history by length alone, so
        // `streams_by_sender`/`streams_by_recipient` pair this with `total`.
        let effective_limit = limit.min(PAGE_SIZE);
        let end = offset.saturating_add(effective_limit).min(count);
        let mut result = Vec::new(env);
        let mut cursor = offset;

        while cursor < end {
            let page_index = cursor / PAGE_SIZE;
            let page_offset = (cursor % PAGE_SIZE) as usize;
            let page = collect_page(env, &mut make_page_key, page_index);
            let page_len = page.len() as usize;

            if page_offset >= page_len {
                let next_page_start = page_index.saturating_add(1).saturating_mul(PAGE_SIZE);
                if next_page_start <= cursor {
                    break;
                }
                cursor = next_page_start;
                continue;
            }

            let remaining_in_page = page_len - page_offset;
            let remaining_total = (end - cursor) as usize;
            let take = remaining_in_page.min(remaining_total);
            for i in page_offset..(page_offset + take) {
                result.push_back(page.get(i as u32).unwrap());
            }
            cursor = cursor.saturating_add(take as u32);
        }

        return result;
    }

    let legacy: Vec<u64> = env
        .storage()
        .persistent()
        .get(legacy_key)
        .unwrap_or(Vec::new(env));
    if env.storage().persistent().has(legacy_key) {
        extend_ttl(env, legacy_key);
    }
    query::paginate(env, legacy, offset, limit)
}

fn count_index(env: &Env, count_key: &DataKey, legacy_key: &DataKey) -> u32 {
    if let Some(count) = env.storage().persistent().get::<_, u32>(count_key) {
        extend_ttl(env, count_key);
        return count;
    }

    let legacy: Vec<u64> = env
        .storage()
        .persistent()
        .get(legacy_key)
        .unwrap_or(Vec::new(env));
    if env.storage().persistent().has(legacy_key) {
        extend_ttl(env, legacy_key);
    }
    legacy.len()
}

pub fn append_sender_index(env: &Env, sender: &Address, stream_id: u64) {
    let count_key = DataKey::BySenderCount(sender.clone());
    let legacy_key = DataKey::BySender(sender.clone());
    append_index_entry(
        env,
        &count_key,
        &legacy_key,
        |page| DataKey::BySenderPage(sender.clone(), page),
        stream_id,
    );
}

pub fn append_recipient_index(env: &Env, recipient: &Address, stream_id: u64) {
    let count_key = DataKey::ByRecipientCount(recipient.clone());
    let legacy_key = DataKey::ByRecipient(recipient.clone());
    append_index_entry(
        env,
        &count_key,
        &legacy_key,
        |page| DataKey::ByRecipientPage(recipient.clone(), page),
        stream_id,
    );
}

pub fn streams_by_sender(env: &Env, sender: Address, offset: u32, limit: u32) -> StreamPage {
    let count_key = DataKey::BySenderCount(sender.clone());
    let legacy_key = DataKey::BySender(sender.clone());
    let ids = read_index(
        env,
        &count_key,
        &legacy_key,
        |page| DataKey::BySenderPage(sender.clone(), page),
        offset,
        limit,
    );
    let total = count_index(env, &count_key, &legacy_key);
    StreamPage { ids, total }
}

pub fn streams_by_recipient(env: &Env, recipient: Address, offset: u32, limit: u32) -> StreamPage {
    let count_key = DataKey::ByRecipientCount(recipient.clone());
    let legacy_key = DataKey::ByRecipient(recipient.clone());
    let ids = read_index(
        env,
        &count_key,
        &legacy_key,
        |page| DataKey::ByRecipientPage(recipient.clone(), page),
        offset,
        limit,
    );
    let total = count_index(env, &count_key, &legacy_key);
    StreamPage { ids, total }
}

pub fn stream_count_by_sender(env: &Env, sender: Address) -> u32 {
    let count_key = DataKey::BySenderCount(sender.clone());
    let legacy_key = DataKey::BySender(sender);
    count_index(env, &count_key, &legacy_key)
}

pub fn stream_count_by_recipient(env: &Env, recipient: Address) -> u32 {
    let count_key = DataKey::ByRecipientCount(recipient.clone());
    let legacy_key = DataKey::ByRecipient(recipient);
    count_index(env, &count_key, &legacy_key)
}
