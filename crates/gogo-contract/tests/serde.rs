//! The catalogue serializes for the dev inspector (`--features serde`).
#![cfg(feature = "serde")]

#[test]
fn catalogue_serializes() {
    let actions = serde_json::to_value(gogo_contract::actions::ACTIONS).expect("actions");
    let update = actions
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"] == "game.update"))
        .expect("game.update");
    assert_eq!(update["tier"], "Confirm");
    assert_eq!(update["escalation"], "DestructiveIfRemovesDlcOrDowngrades");
    for value in [
        serde_json::to_value(gogo_contract::sources::SOURCES),
        serde_json::to_value(gogo_contract::widgets::WIDGETS),
        serde_json::to_value(gogo_contract::routes::ROUTES),
        serde_json::to_value(gogo_contract::Invariant::ALL),
        serde_json::to_value(gogo_contract::ContractVersion::CURRENT),
    ] {
        assert!(value.is_ok());
    }
}
