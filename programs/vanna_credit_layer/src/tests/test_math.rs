mod fixed_point {
    use vanna_credit_layer::math::fixed_point::*;

    #[test]
    fn floor_and_ceil_agree_on_exact_division() {
        assert_eq!(mul_div_floor(10, 5, 2).unwrap(), 25);
        assert_eq!(mul_div_ceil(10, 5, 2).unwrap(), 25);
    }

    #[test]
    fn ceil_rounds_up_on_remainder() {
        assert_eq!(mul_div_floor(7, 1, 2).unwrap(), 3);
        assert_eq!(mul_div_ceil(7, 1, 2).unwrap(), 4);
    }

    #[test]
    fn rejects_division_by_zero() {
        assert!(mul_div_floor(1, 1, 0).is_err());
        assert!(mul_div_ceil(1, 1, 0).is_err());
    }

    #[test]
    fn pow10_bounds() {
        assert_eq!(checked_pow10(0).unwrap(), 1);
        assert_eq!(checked_pow10(9).unwrap(), 1_000_000_000);
        assert!(checked_pow10(39).is_err());
    }
}

mod health {
    use vanna_credit_layer::constants::BALANCE_TO_BORROW_THRESHOLD_WAD;
    use vanna_credit_layer::math::health::*;
    use vanna_oracle::price::normalize_token_value;

    #[test]
    fn normalize_handles_negative_exponent_and_rounding() {
        let value = normalize_token_value(1_000_000, 100_000_000, -8, 6, false).unwrap();
        assert_eq!(value, 1_000_000_000);
    }

    #[test]
    fn normalize_rounds_up_for_debt() {
        let down = normalize_token_value(3, 100_000_000, -8, 6, false).unwrap();
        let up = normalize_token_value(3, 100_000_000, -8, 6, true).unwrap();
        assert!(up >= down);
    }

    #[test]
    fn rejects_non_positive_price() {
        assert!(normalize_token_value(1_000_000, 0, -8, 6, false).is_err());
        assert!(normalize_token_value(1_000_000, -1, -8, 6, false).is_err());
    }

    #[test]
    fn zero_debt_is_infinitely_healthy() {
        let snap = calculate_health(&[], &[]).unwrap();
        assert!(snap.is_borrow_healthy());
        assert!(!snap.is_liquidatable());
        assert_eq!(snap.borrow_health_factor_wad, u128::MAX);
    }

    #[test]
    fn health_ratio_overflow_fails_closed() {
        let result = calculate_health(
            &[CollateralValuation {
                collateral_value: u128::MAX,
            }],
            &[DebtValuation { debt_value: 1 }],
        );
        assert!(result.is_err());
    }

    #[test]
    fn health_uses_raw_collateral_and_single_reference_threshold() {
        let collaterals = [CollateralValuation {
            collateral_value: 1_000_000_000,
        }];
        let debts = [DebtValuation { debt_value: 900_000_000 }];
        let snap = calculate_health(&collaterals, &debts).unwrap();
        assert_eq!(snap.borrow_power, 1_000_000_000);
        assert_eq!(snap.liquidation_collateral_value, 1_000_000_000);
        assert!(snap.is_borrow_healthy());
        assert!(!snap.is_liquidatable());
    }

    #[test]
    fn equality_at_reference_threshold_is_unhealthy_and_liquidatable() {
        let collaterals = [CollateralValuation {
            collateral_value: 1_100_000_000,
        }];
        let debts = [DebtValuation { debt_value: 1_000_000_000 }];
        let snap = calculate_health(&collaterals, &debts).unwrap();
        assert_eq!(snap.borrow_health_factor_wad, BALANCE_TO_BORROW_THRESHOLD_WAD);
        assert!(!snap.is_borrow_healthy());
        assert!(snap.is_liquidatable());
    }

    #[test]
    fn projected_leverage_matches_solidity_and_soroban() {
        let five_x = calculate_health(
            &[CollateralValuation {
                collateral_value: 50_000_000_000,
            }],
            &[DebtValuation {
                debt_value: 40_000_000_000,
            }],
        )
        .unwrap();
        assert_eq!(five_x.borrow_health_factor_wad, 1_250_000_000_000_000_000);
        assert!(five_x.is_borrow_healthy());

        let ten_x = calculate_health(
            &[CollateralValuation {
                collateral_value: 100_000_000_000,
            }],
            &[DebtValuation {
                debt_value: 90_000_000_000,
            }],
        )
        .unwrap();
        assert!(ten_x.is_borrow_healthy());

        let eleven_x = calculate_health(
            &[CollateralValuation {
                collateral_value: 110_000_000_000,
            }],
            &[DebtValuation {
                debt_value: 100_000_000_000,
            }],
        )
        .unwrap();
        assert!(!eleven_x.is_borrow_healthy());
        assert!(eleven_x.is_liquidatable());
    }
}

mod shares {
    use vanna_credit_layer::math::shares::*;

    #[test]
    fn first_depositor_gets_assets_as_shares() {
        assert_eq!(assets_to_supply_shares_down(1_000, 0, 0).unwrap(), 1_000);
    }

    #[test]
    fn later_depositor_shares_scale_with_exchange_rate() {
        assert_eq!(assets_to_supply_shares_down(100, 1_000, 2_000).unwrap(), 50);
    }

    #[test]
    fn redeem_rounds_down() {
        assert_eq!(supply_shares_to_assets_down(3, 10, 7).unwrap(), 2);
    }

    #[test]
    fn first_borrow_gets_assets_as_shares() {
        assert_eq!(assets_to_debt_shares_up(500, 0, 0).unwrap(), 500);
    }

    #[test]
    fn later_borrow_rounds_up() {
        assert_eq!(assets_to_debt_shares_up(1, 1, 3).unwrap(), 1);
    }

    #[test]
    fn debt_value_rounds_up() {
        assert_eq!(debt_shares_to_assets_up(1, 3, 7).unwrap(), 3);
    }

    #[test]
    fn zero_total_shares_implies_zero_position_debt() {
        assert_eq!(debt_shares_to_assets_up(0, 0, 0).unwrap(), 0);
    }
}
