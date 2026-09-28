//! Unit tests for the margin account's active-asset registries.

mod margin_account {
    use anchor_lang::prelude::Pubkey;
    use vanna_lending::constants::MAX_ASSETS;
    use vanna_lending::state::margin_account::MarginAccount;

    #[test]
    fn add_and_remove_collateral_roundtrips() {
        let mut m = MarginAccount::new_empty(Pubkey::new_unique(), 255);
        m.add_active_collateral(3).unwrap();
        assert!(m.is_collateral_active(3));
        assert_eq!(m.collateral_count, 1);
        m.remove_active_collateral(3).unwrap();
        assert!(!m.is_collateral_active(3));
        assert_eq!(m.collateral_count, 0);
    }

    #[test]
    fn rejects_duplicate_active_index() {
        let mut m = MarginAccount::new_empty(Pubkey::new_unique(), 255);
        m.add_active_debt(1).unwrap();
        assert!(m.add_active_debt(1).is_err());
    }

    #[test]
    fn rejects_beyond_max_assets() {
        let mut m = MarginAccount::new_empty(Pubkey::new_unique(), 255);
        for i in 0..MAX_ASSETS as u16 {
            m.add_active_collateral(i).unwrap();
        }
        assert!(m.add_active_collateral(MAX_ASSETS as u16).is_err());
    }
}
