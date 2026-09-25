//! Unit tests for account-state helpers: margin asset registries and lite debt attribution.

mod margin_account {
    use anchor_lang::prelude::Pubkey;
    use vanna_lending::constants::MAX_ASSETS;
    use vanna_lending::state::margin_account::MarginAccount;

    #[test]
    fn lite_registry_preserves_layout_and_removes_only_target() {
        let mut m = MarginAccount::new_empty(Pubkey::new_unique(), 255);
        assert!(m.lite_indexes().unwrap().is_empty());
        m.register_lite(3).unwrap();
        m.register_lite(2).unwrap();
        m.register_lite(3).unwrap();
        assert_eq!(m.lite_indexes().unwrap(), vec![3, 2]);
        assert!(!m.is_empty());
        m.unregister_lite(3).unwrap();
        assert_eq!(m.lite_indexes().unwrap(), vec![2]);
        m.unregister_lite(2).unwrap();
        assert!(m.is_empty());
        assert_eq!(m.reserved, [0u8; 96]);
    }

    #[test]
    fn lite_registry_rejects_overflow_and_corruption() {
        let mut m = MarginAccount::new_empty(Pubkey::new_unique(), 255);
        for i in 0..MAX_ASSETS as u16 {
            m.register_lite(i).unwrap();
        }
        assert!(m.register_lite(MAX_ASSETS as u16).is_err());
        m.reserved[0] = (MAX_ASSETS + 1) as u8;
        assert!(m.lite_indexes().is_err());
        m.reserved[0] = 2;
        m.reserved[3] = m.reserved[1];
        assert!(m.lite_indexes().is_err());
    }

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

mod lite_position {
    use anchor_lang::prelude::Pubkey;
    use vanna_lending::state::lite_strategy::LitePosition;

    #[test]
    fn debt_attribution_preserves_layout_and_excludes_other_borrows() {
        let mut p = LitePosition {
            margin_account: Pubkey::default(),
            strategy_config: Pubkey::default(),
            underlying_mint: Pubkey::default(),
            kamino_collateral_amount: 40,
            deposited_underlying: 40,
            equity_underlying: 20,
            bump: 0,
            reserved: [0; 64],
        };
        assert_eq!(p.debt_shares(30), 30); // legacy conservative debt
        p.set_debt_shares(20);
        assert_eq!(p.debt_shares(30), 20);
        assert_eq!(p.debt_shares(10), 10); // debt repaid externally
        p.set_debt_shares(0);
        assert_eq!(p.debt_shares(30), 0);
    }

    /// Mirrors `lite_supply`'s attribution update (`current + attribute_shares_delta`, capped on
    /// read) for a fresh position and a top-up.
    #[test]
    fn debt_attribution_accumulates_across_leveraged_supply_top_ups() {
        let mut p = LitePosition {
            margin_account: Pubkey::default(),
            strategy_config: Pubkey::default(),
            underlying_mint: Pubkey::default(),
            kamino_collateral_amount: 0,
            deposited_underlying: 0,
            equity_underlying: 0,
            bump: 0,
            reserved: [0; 64],
        };
        // Fresh position, first leveraged supply attributes exactly the borrowed shares.
        let current = p.debt_shares(1_000);
        p.set_debt_shares(current + 40);
        assert_eq!(p.debt_shares(1_000), 40);

        // A second leveraged supply (top-up) adds on top rather than overwriting.
        let current = p.debt_shares(1_000);
        p.set_debt_shares(current + 25);
        assert_eq!(p.debt_shares(1_000), 65);

        // Attribution is still capped by whatever's actually outstanding on the reserve.
        assert_eq!(p.debt_shares(50), 50);
    }
}
