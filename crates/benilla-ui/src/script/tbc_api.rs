//! The build-8606 engine verbs the stock 2.4.3 FrameXML calls that have no 1.12 analogue, answered
//! by the offline game. Each is a plain `_G` write, so a FrameXML file that defines the same name
//! later replaces it: these only fill the gap while a kit or a short manifest stops short of that
//! file. The offline game has no battleground, arena, voice chat, LFG queue, tracking or title
//! store, so each answers the reference's "nothing" arm — nil, 0 or an empty table.

use mlua::{Lua, MultiValue, Value};

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // The reference's nil-valued predicates: no such state in the offline world.
    for name in [
        "UnitInBattleground",
        "GetReadyCheckStatus",
        "UnitIsPossessed",
        "UnitIsTalking",
        "IsVoiceChatEnabled",
        "GetMirrorTimerInfo",
        "GetArenaTeam",
        "GetTotemInfo",
        "GetCurrentTitle",
        "GetLFGTypes",
        "GetLFGTypeEntries",
        "IsReferAFriendLinked",
        "GetVoiceCurrentSessionID",
        "GetVoiceSessionInfo",
        "IsVoiceChatAllowedByServer",
        "IsActiveBattlefieldArena",
        "GetLeftFrame",
        "CanGuildBankRepair",
        "IsFalling",
        "GetOptOutOfLoot",
        "GetChannelDisplayInfo",
        "GetTrackingInfo",
        "IsSelectedSpell",
    ] {
        g.set(
            name,
            lua.create_function(|_, _: MultiValue| Ok(Value::Nil))?,
        )?;
    }

    // The reference's zero-valued queries.
    for name in [
        "GetNumVoiceSessions",
        "GetSelectedDisplayChannel",
        "GetNumLanguages",
        "Sound_GameSystem_GetNumOutputDrivers",
        "Sound_ChatSystem_GetNumInputDrivers",
        "Sound_ChatSystem_GetNumOutputDrivers",
        "VoiceEnumerateOutputDevices",
        "VoiceEnumerateCaptureDevices",
        "GetNumDisplayChannels",
        "GetNumTitles",
        "GetHonorCurrency",
        "GetCritChanceFromAgility",
        "GetNumTrackingTypes",
        "GetNumVoiceSessionMembersBySessionID",
        "GetArenaCurrency",
        "GetSpellCritChanceFromIntellect",
    ] {
        g.set(name, lua.create_function(|_, _: MultiValue| Ok(0i64))?)?;
    }

    // The reference's void actions.
    for name in [
        "InitWorldMapPing",
        "SecureUnitButton_OnLoad",
        "SetAutoLootDefault",
        "ClearLFMAutofill",
        "ClearLFGAutojoin",
        "CancelSummon",
    ] {
        g.set(name, lua.create_function(|_, _: MultiValue| Ok(()))?)?;
    }

    // `GetCurrentDungeonDifficulty()` (`0x4c9d20`): the offline game has one difficulty, normal (1).
    g.set(
        "GetCurrentDungeonDifficulty",
        lua.create_function(|_, _: MultiValue| Ok(1i64))?,
    )?;

    // `Sound_GameSystem_GetOutputDriverNameByIndex(i)` (`0x77e7e0`): no driver list, no name.
    g.set(
        "Sound_GameSystem_GetOutputDriverNameByIndex",
        lua.create_function(|_, _: MultiValue| Ok(Value::Nil))?,
    )?;

    // `GetUnitMaxHealthModifier(unit)` (`0x51d1a0`): the health multiplier `PaperDollFrame.lua:236`
    // folds into the stamina tooltip; 1 is the unmodified body.
    g.set(
        "GetUnitMaxHealthModifier",
        lua.create_function(|_, _: MultiValue| Ok(1.0f64))?,
    )?;

    // `SecureButton_GetModifiedAttribute(button, attribute)` (`SecureTemplates.lua`): the button's
    // own attribute, the identity leg.
    g.set(
        "SecureButton_GetModifiedAttribute",
        lua.create_function(|_, (button, attr): (mlua::Table, String)| button.get::<Value>(attr))?,
    )?;

    // `SecureButton_GetEffectiveButton(button)` (`SecureTemplates.lua`): the button itself, the
    // identity leg while no `effectiveButton` attribute is set.
    g.set(
        "SecureButton_GetEffectiveButton",
        lua.create_function(|_, button: Value| Ok(button))?,
    )?;

    // `GetExistingLocales()` (`0x4be5e0`): the client's locale list as the VARARG
    // `UIOptionsPanels.lua:828` walks with `select("#", ...)`; enUS alone, as benilla ships.
    g.set(
        "GetExistingLocales",
        lua.create_function(|lua, ()| {
            let mut out = MultiValue::new();
            out.push_back(Value::String(lua.create_string("enUS")?));
            Ok(out)
        })?,
    )?;

    // `GetAttackPowerForStat(stat, unit)` (`0x51d1a0`): 0 until the stat model lands.
    g.set(
        "GetAttackPowerForStat",
        lua.create_function(|_, _: MultiValue| Ok(0i64))?,
    )?;

    // `MicroButtonTooltipText(text, action)` (`MicroButton.lua`): the plain text, the reference's
    // non-keybinding arm.
    g.set(
        "MicroButtonTooltipText",
        lua.create_function(|_, text: Option<String>| Ok(text.unwrap_or_default()))?,
    )?;

    Ok(())
}
