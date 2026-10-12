use super::*;

/// The session's vendor buyback ledger: the last 12 items sold, oldest first, session-only (a
/// relog clears it, as the reference's window does). Switching vendors clears it.
#[derive(Resource, Default)]
pub(super) struct OfflineVendorBuyback {
    vendor: Option<u64>,
    sold: Vec<OfflineBuybackItem>,
    next_timestamp: u32,
}

struct OfflineBuybackItem {
    /// The presentation GUID the item had while carried; the buyback object keeps it.
    guid: u64,
    entry: u32,
    count: u32,
    price: u32,
    timestamp: u32,
}

impl OfflineVendorBuyback {
    /// Adopt `vendor`; a different vendor's visit starts a fresh ledger.
    fn open_vendor(&mut self, vendor: u64) {
        if self.vendor != Some(vendor) {
            self.vendor = Some(vendor);
            self.sold.clear();
        }
    }

    fn push(&mut self, mut item: OfflineBuybackItem) {
        if self.sold.len() == OfflineVendorBuyback::CAPACITY {
            self.sold.remove(0);
        }
        self.next_timestamp = self.next_timestamp.saturating_add(1);
        item.timestamp = self.next_timestamp;
        self.sold.push(item);
    }

    fn take_slot(&mut self, index: usize) -> Option<OfflineBuybackItem> {
        (index < self.sold.len()).then(|| self.sold.remove(index))
    }

    /// The 12 slots, prices and timestamps as a descriptor delta: every slot is written, so a
    /// vacated one clears live.
    fn field_pairs(&self) -> Vec<(u16, u32)> {
        let mut pairs = Vec::with_capacity(48);
        for index in 0..OfflineVendorBuyback::CAPACITY {
            let item = self.sold.get(index);
            let guid = item.map_or(0, |item| item.guid);
            let slot = FIELD_PLAYER_VENDORBUYBACK_SLOT_1 + 2 * index as u16;
            pairs.push((slot, guid as u32));
            pairs.push((slot + 1, (guid >> 32) as u32));
            pairs.push((
                FIELD_PLAYER_BUYBACK_PRICE_1 + index as u16,
                item.map_or(0, |item| item.price),
            ));
            pairs.push((
                FIELD_PLAYER_BUYBACK_TIMESTAMP_1 + index as u16,
                item.map_or(0, |item| item.timestamp),
            ));
        }
        pairs
    }

    const CAPACITY: usize = 12;
}

/// `PLAYER_FIELD_VENDORBUYBACK_SLOT_1`: 12 guid pairs, indexed slot−69.
const FIELD_PLAYER_VENDORBUYBACK_SLOT_1: u16 = 624;
/// `PLAYER_BUYBACK_PRICE_1`: 12 u32 copper.
const FIELD_PLAYER_BUYBACK_PRICE_1: u16 = 1226;
/// `PLAYER_BUYBACK_TIMESTAMP_1`: 12 u32 sort keys.
const FIELD_PLAYER_BUYBACK_TIMESTAMP_1: u16 = 1238;
/// `BUYBACK_SLOT_START`: the first absolute buyback slot on the wire (69-80).
pub(super) const BUYBACK_SLOT_FIRST: u32 = 69;
/// A vendor row's unlimited `current_count` (vmangos `ItemHandler.cpp:795`).
pub(super) const VENDOR_STOCK_UNLIMITED: u32 = 0xFFFF_FFFF;

/// The spawned NPC's converted template, for its service flags and vendor entry.
pub(super) fn imported_template_for_guid(
    guid: u64,
) -> Option<&'static benilla_game_core::content::CreatureTemplate> {
    let spawn = super::imported_spawn_for_guid(guid)?;
    benilla_game_core::content::creature_content().template(spawn.entry)
}

/// Whether the guid's spawn is within the client's 5.5556 yd service range of the active player.
fn offline_npc_in_range(core: &GameCore, guid: u64) -> bool {
    let Some(spawn) = super::imported_spawn_for_guid(guid) else {
        return false;
    };
    let Some(player) = core
        .active()
        .and_then(|id| core.snapshot(id))
        .map(|snapshot| snapshot.position)
    else {
        return false;
    };
    player.map_id == spawn.map_id
        && (player.x - spawn.x).powi(2)
            + (player.y - spawn.y).powi(2)
            + (player.z - spawn.z).powi(2)
            <= super::OFFLINE_INTERACTION_RANGE_YARDS * super::OFFLINE_INTERACTION_RANGE_YARDS
}

/// The carried item `guid` names, as a core position (bag or worn slot).
fn inventory_slot_for_guid(
    snapshot: &benilla_game_core::CharacterSnapshot,
    guid: u64,
) -> Option<benilla_game_core::ItemSlot> {
    snapshot
        .inventory
        .iter()
        .find(|item| super::inventory_item_guid(snapshot, item) == guid)
        .map(|item| benilla_game_core::ItemSlot::Backpack(item.slot))
        .or_else(|| {
            snapshot
                .equipment
                .iter()
                .find(|item| super::inventory_item_guid(snapshot, item) == guid)
                .map(|item| benilla_game_core::ItemSlot::Equipment(item.slot))
        })
}

/// The descriptor delta a vendor trade publishes: the item slots, the purse, and the buyback
/// ledger when the session has one.
fn vendor_player_fields(
    snapshot: &benilla_game_core::CharacterSnapshot,
    buyback: Option<&OfflineVendorBuyback>,
) -> Vec<(u16, u32)> {
    let mut fields = super::player_item_field_pairs(snapshot);
    fields.push((1176, snapshot.money_copper)); // PLAYER_FIELD_COINAGE
    if let Some(state) = buyback {
        fields.extend(state.field_pairs());
    }
    fields
}

/// `SMSG_LIST_INVENTORY`: the converted stock lines with `item_template` prices and display ids,
/// plus the templates themselves so names resolve without a query round-trip.
pub(super) fn send_offline_vendor_inventory(
    core: &GameCore,
    channels: &OfflineChannels,
    buyback: Option<&mut OfflineVendorBuyback>,
    guid: u64,
) {
    let Some(template) = imported_template_for_guid(guid) else {
        return;
    };
    let rows: Vec<_> = benilla_game_core::vendor::vendor_items(template.entry).collect();
    if rows.is_empty() || !offline_npc_in_range(core, guid) {
        return;
    }
    if let Some(state) = buyback {
        state.open_vendor(guid);
    }
    let items: Vec<_> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let item = benilla_game_core::quest::item_template(row.item);
            benilla_protocol::messages::VendorItem {
                slot: u32::try_from(index + 1).unwrap_or(u32::MAX),
                entry: row.item,
                display_id: item.map_or(0, |item| item.display_id),
                current_count: VENDOR_STOCK_UNLIMITED,
                price: item.map_or(0, |item| item.buy_price),
                max_durability: item.map_or(0, |item| item.max_durability),
                buy_count: row.buy_count.max(1),
            }
        })
        .collect();
    let mut events: Vec<_> = items
        .iter()
        .filter_map(|item| {
            super::item_info(item.entry).map(|info| SessionEvent::ItemTemplate {
                entry: item.entry,
                info: Some(Box::new(info)),
            })
        })
        .collect();
    events.push(SessionEvent::VendorInventory {
        vendor: guid,
        items,
    });
    super::send_gameplay_events(channels, events);
}

/// A vendor row for `entry`, by its 1-based display slot.
fn vendor_row_index(template_entry: u32, item: u32) -> Option<usize> {
    benilla_game_core::vendor::vendor_items(template_entry).position(|row| row.item == item)
}

/// `CMSG_BUY_ITEM`/`CMSG_BUY_ITEM_IN_SLOT` offline: spend the purse, grant the item, and answer
/// with the same events the server would (`SMSG_BUY_ITEM`, the item create, the coinage update).
#[allow(clippy::too_many_arguments)]
pub(super) fn buy_offline_vendor_item(
    core: &mut GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    buyback: Option<&mut OfflineVendorBuyback>,
    vendor: u64,
    entry: u32,
    count: u8,
    at: Option<(u64, u8)>,
) {
    let fail = |channels: &OfflineChannels, reason: u8| {
        super::send_gameplay_events(
            channels,
            [SessionEvent::VendorBuyFailed {
                vendor,
                item_entry: entry,
                reason,
            }],
        );
    };
    let Some(template) = imported_template_for_guid(vendor) else {
        return;
    };
    if !offline_npc_in_range(core, vendor) {
        fail(channels, buy_result::DISTANCE_TOO_FAR);
        return;
    }
    let Some(index) = vendor_row_index(template.entry, entry) else {
        fail(channels, buy_result::CANT_FIND_ITEM);
        return;
    };
    let Some(item_template) = benilla_game_core::quest::item_template(entry) else {
        fail(channels, buy_result::CANT_FIND_ITEM);
        return;
    };
    let buy_count = benilla_game_core::vendor::vendor_items(template.entry)
        .nth(index)
        .map_or(1, |row| row.buy_count.max(1));
    let stacks = u32::from(count.max(1));
    let units = stacks.saturating_mul(buy_count);
    let price = item_template.buy_price.saturating_mul(stacks);
    if core.spend_money(price).is_err() {
        fail(channels, buy_result::NOT_ENOUGH_MONEY);
        return;
    }
    // `CMSG_BUY_ITEM_IN_SLOT` names the target container's guid: ours for the player's own array
    // (the bag byte itself is not on the command), so a bag we do not model buys into the first
    // free slot instead.
    let player_guid = core.active().map(|id| id.presentation_guid());
    let at_slot = at.and_then(|(bag_guid, slot)| {
        (Some(bag_guid) == player_guid)
            .then(|| super::wire_item_slot(super::BAG_PLAYER_INVENTORY, slot))
            .flatten()
    });
    let item = match core.grant_item(entry, units, at_slot) {
        Ok(item) => item,
        Err(_) => {
            let _ = core.credit_money(price);
            fail(channels, buy_result::CANT_CARRY_MORE);
            return;
        }
    };
    let Some(id) = core.active().cloned() else {
        return;
    };
    let Some(snapshot) = core.snapshot(&id) else {
        return;
    };
    let player_guid = id.presentation_guid();
    let mut events = Vec::new();
    if let Some(info) = super::item_info(entry) {
        events.push(SessionEvent::ItemTemplate {
            entry,
            info: Some(Box::new(info)),
        });
    }
    events.push(super::item_create_event(player_guid, snapshot, &item));
    events.push(SessionEvent::ObjectValues {
        guid: player_guid,
        fields: ObjectFields::from_pairs(&vendor_player_fields(snapshot, buyback.as_deref())),
    });
    events.push(SessionEvent::ItemPushResult(
        benilla_protocol::messages::ItemPushResult {
            player_guid,
            from_npc: false,
            created: false,
            show_in_chat: true,
            bag_slot: super::BAG_PLAYER_INVENTORY,
            item_slot: u32::from(item.slot),
            item_entry: item.entry,
            suffix_factor: 0,
            random_property_id: 0,
            count: item.count,
        },
    ));
    events.push(SessionEvent::VendorBuyResult {
        vendor,
        slot: u32::try_from(index + 1).unwrap_or(u32::MAX),
        new_count: VENDOR_STOCK_UNLIMITED,
        purchase_count: units,
    });
    super::send_gameplay_events(channels, events);
    super::save_after_command(core, persistence);
}

/// `CMSG_SELL_ITEM` offline: remove the stack (`count` 0) or part of it, credit
/// `sell_price × units`, and keep the sold item on the buyback ledger.
#[allow(clippy::too_many_arguments)]
pub(super) fn sell_offline_item(
    core: &mut GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    mut buyback: Option<&mut OfflineVendorBuyback>,
    vendor: u64,
    item_guid: u64,
    count: u8,
) {
    let fail = |channels: &OfflineChannels, reason: u8| {
        super::send_gameplay_events(
            channels,
            [SessionEvent::VendorSellFailed {
                vendor,
                item_guid,
                reason,
            }],
        );
    };
    if imported_template_for_guid(vendor).is_none() {
        return;
    }
    if !offline_npc_in_range(core, vendor) {
        fail(channels, sell_result::CANT_FIND_VENDOR);
        return;
    }
    let Some(id) = core.active().cloned() else {
        return;
    };
    let Some(snapshot) = core.snapshot(&id) else {
        return;
    };
    let Some(at) = inventory_slot_for_guid(snapshot, item_guid) else {
        fail(channels, sell_result::CANT_FIND_ITEM);
        return;
    };
    let Ok(item) = core.take_item(at) else {
        fail(channels, sell_result::CANT_FIND_ITEM);
        return;
    };
    let sold_units = if count == 0 {
        item.count
    } else {
        u32::from(count).min(item.count)
    };
    if sold_units == 0 {
        let _ = core.grant_item(item.entry, item.count, Some(at));
        return;
    }
    let unit_price = benilla_game_core::quest::item_template(item.entry)
        .map_or(0, |template| template.sell_price);
    let proceeds = unit_price.saturating_mul(sold_units);
    let remainder = (item.count > sold_units)
        .then(|| {
            core.grant_item(item.entry, item.count - sold_units, Some(at))
                .ok()
        })
        .flatten();
    let _ = core.credit_money(proceeds);
    if let Some(state) = buyback.as_deref_mut() {
        state.open_vendor(vendor);
        state.push(OfflineBuybackItem {
            guid: item_guid,
            entry: item.entry,
            count: sold_units,
            price: proceeds,
            timestamp: 0,
        });
    }
    let Some(snapshot) = core.snapshot(&id) else {
        return;
    };
    let player_guid = id.presentation_guid();
    let mut events = Vec::new();
    if let Some(info) = super::item_info(item.entry) {
        events.push(SessionEvent::ItemTemplate {
            entry: item.entry,
            info: Some(Box::new(info)),
        });
    }
    if let Some(remainder) = &remainder {
        events.push(super::item_create_event(player_guid, snapshot, remainder));
    }
    events.push(SessionEvent::ObjectValues {
        guid: player_guid,
        fields: ObjectFields::from_pairs(&vendor_player_fields(snapshot, buyback.as_deref())),
    });
    if buyback.as_deref().is_some() {
        // The sold item's object stays streamed under its old GUID, carrying the sold count, so
        // the buyback row resolves.
        let max_durability = super::item_info(item.entry).map_or(0, |info| info.max_durability);
        events.push(SessionEvent::ItemCreate {
            guid: item_guid,
            container: false,
            fields: ObjectFields::from_pairs(&[
                (2, 0x3),
                (3, item.entry),
                (4, 1.0f32.to_bits()),
                (6, player_guid as u32),
                (7, (player_guid >> 32) as u32),
                (8, player_guid as u32),
                (9, (player_guid >> 32) as u32),
                (14, sold_units),
                (46, max_durability),
                (47, max_durability),
            ])
            .into_created(benilla_protocol::messages::ObjectType::Item),
        });
    }
    super::send_gameplay_events(channels, events);
    super::save_after_command(core, persistence);
}

/// `CMSG_BUYBACK_ITEM` offline: reclaim the slot's item at the price it sold for.
pub(super) fn buyback_offline_item(
    core: &mut GameCore,
    persistence: &mut OfflinePersistence,
    channels: &OfflineChannels,
    buyback: Option<&mut OfflineVendorBuyback>,
    vendor: u64,
    slot: u32,
) {
    let Some(state) = buyback else {
        return;
    };
    if state.vendor != Some(vendor) {
        return;
    }
    let Some(index) = slot
        .checked_sub(BUYBACK_SLOT_FIRST)
        .and_then(|index| usize::try_from(index).ok())
    else {
        return;
    };
    let Some(record) = state.take_slot(index) else {
        return;
    };
    let OfflineBuybackItem {
        guid,
        entry,
        count,
        price,
        ..
    } = record;
    if !offline_npc_in_range(core, vendor) {
        state.push(OfflineBuybackItem {
            guid,
            entry,
            count,
            price,
            timestamp: 0,
        });
        return;
    }
    if core.spend_money(price).is_err() {
        state.push(OfflineBuybackItem {
            guid,
            entry,
            count,
            price,
            timestamp: 0,
        });
        super::send_gameplay_events(
            channels,
            [SessionEvent::VendorBuyFailed {
                vendor,
                item_entry: entry,
                reason: buy_result::NOT_ENOUGH_MONEY,
            }],
        );
        return;
    }
    match core.grant_item(entry, count, None) {
        Ok(item) => {
            let Some(id) = core.active().cloned() else {
                return;
            };
            let Some(snapshot) = core.snapshot(&id) else {
                return;
            };
            let player_guid = id.presentation_guid();
            let mut events = Vec::new();
            if let Some(info) = super::item_info(entry) {
                events.push(SessionEvent::ItemTemplate {
                    entry,
                    info: Some(Box::new(info)),
                });
            }
            events.push(SessionEvent::ObjectDestroyed(guid));
            events.push(super::item_create_event(player_guid, snapshot, &item));
            events.push(SessionEvent::ObjectValues {
                guid: player_guid,
                fields: ObjectFields::from_pairs(&vendor_player_fields(snapshot, Some(state))),
            });
            events.push(SessionEvent::ItemPushResult(
                benilla_protocol::messages::ItemPushResult {
                    player_guid,
                    from_npc: false,
                    created: false,
                    show_in_chat: true,
                    bag_slot: super::BAG_PLAYER_INVENTORY,
                    item_slot: u32::from(item.slot),
                    item_entry: item.entry,
                    suffix_factor: 0,
                    random_property_id: 0,
                    count: item.count,
                },
            ));
            super::send_gameplay_events(channels, events);
            super::save_after_command(core, persistence);
        }
        Err(_) => {
            let _ = core.credit_money(price);
            state.push(OfflineBuybackItem {
                guid,
                entry,
                count,
                price,
                timestamp: 0,
            });
            super::send_gameplay_events(
                channels,
                [SessionEvent::VendorBuyFailed {
                    vendor,
                    item_entry: entry,
                    reason: buy_result::CANT_CARRY_MORE,
                }],
            );
        }
    }
}
