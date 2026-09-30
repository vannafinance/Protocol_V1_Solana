pub mod known_mints {
    pub const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    pub const USDT: &str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";
    pub const WSOL: &str = "So11111111111111111111111111111111111111112";
    pub const JITOSOL: &str = "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn";
    pub const JUPSOL: &str = "jupSoLaHXQiZZTSfEWMTRRgpnyFm8f6sZdosWBjx93v";
    pub const JUPUSD: &str = "JuprjznTrTSp2UFa3ZBUFgwdAmtZCq4MQCwysN55USD";
    pub const NVDAX: &str = "Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh";
    pub const TSLAX: &str = "XsDoVfqeBukxuZHWhdvWHBhgEHjGNst4MLodqsJHzoB";
}

pub mod pyth_feeds {
    pub const USDC_USD: &str = "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a";
    pub const USDT_USD: &str = "2b89b9dc8fdf9f34709a5b106b472f0f39bb6ca9ce04b0fd7f2e971688e2e53b";
    pub const SOL_USD: &str = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
    pub const JUPSOL_SOL_RR: &str = "f8d8d6b6c866c8b2624fb5b679ae846738725e5fc887fa8e927c8d8645018a2b";
    pub const JUPUSD_USD: &str = "8ed858a2214e892c9371694fb6c8a9037b6ed4052c4edf209f8cb988484e81d9";
}

pub mod kamino_scope {
    pub const ORACLE_PRICES: &str = "3t4JZcueEzTbVP6kLxXrL3VpWx45jDer4eqysweBchNH";
    pub const USDC: ([u16; 4], [u16; 4]) = ([13, u16::MAX, u16::MAX, u16::MAX], [456, u16::MAX, u16::MAX, u16::MAX]);
    pub const USDT: ([u16; 4], [u16; 4]) = ([16, u16::MAX, u16::MAX, u16::MAX], [457, u16::MAX, u16::MAX, u16::MAX]);
    pub const SOL: ([u16; 4], [u16; 4]) = ([3, u16::MAX, u16::MAX, u16::MAX], [455, u16::MAX, u16::MAX, u16::MAX]);
    pub const JITOSOL: ([u16; 4], [u16; 4]) = ([210, 3, u16::MAX, u16::MAX], [210, 455, u16::MAX, u16::MAX]);
    pub const JUPSOL: ([u16; 4], [u16; 4]) = ([224, 3, u16::MAX, u16::MAX], [224, 455, u16::MAX, u16::MAX]);
    pub const NVDAX: ([u16; 4], [u16; 4]) = ([332, u16::MAX, u16::MAX, u16::MAX], [269, u16::MAX, u16::MAX, u16::MAX]);
    pub const TSLAX: ([u16; 4], [u16; 4]) = ([338, u16::MAX, u16::MAX, u16::MAX], [273, u16::MAX, u16::MAX, u16::MAX]);
}

pub mod kamino_main_market {
    pub const KLEND_PROGRAM: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
    pub const MARKET: &str = "7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF";
    pub const MARKET_AUTHORITY: &str = "9DrvZvyWh1HuAoZxvYWMvkf2XCzryCpGgHqrMjyDWpmo";

    pub mod sol {
        pub const RESERVE: &str = "d4A2prbA2whesmvHaL88BH6Ewn5N4bTSU2Ze8P6Bc4Q";
        pub const LIQUIDITY_VAULT: &str = "GafNuUXj9rxGLn4y79dPu6MHSuPWeJR6UtTWuexpGh3U";
        pub const COLLATERAL_MINT: &str = "2UywZrUdyqs5vDchy7fKQJKau2RVyuzBev2XKGPDSiX1";
    }

    pub mod usdc {
        pub const RESERVE: &str = "D6q6wuQSrifJKZYpR1M8R4YawnLDtDsMmWM1NbBmgJ59";
        pub const LIQUIDITY_VAULT: &str = "Bgq7trRgVMeq33yt235zM2onQ4bRDBsY5EWiTetF4qw6";
        pub const COLLATERAL_MINT: &str = "B8V6WVjPxW1UGwVDfxH2d2r8SyT4cqn7dQRK6XneVa7D";
    }
}

pub mod gmtrade {
    pub const STORE_PROGRAM: &str = "Gmso1uvJnLbawvw7yezdfCDcPydwW2s2iqG3w6MDucLo";
    pub const STORE: &str = "CTDLvGGXnoxvqLyTpGzdGLg9pD6JexKxKXSV8tqqo8bN";
    pub mod eth {
        pub const MARKET: &str = "6EnZdBzJsGznoh857PuhbrnrzWYGtZe6xMZiQjAPyFGT";
        pub const MARKET_TOKEN: &str = "DAY6Qr1FKgJQFvjJAhFUZUWHzx8UbbbkRmt6G6AYswWG";
        pub const INDEX_TOKEN: &str = "EthK4kKnQQUd1Ae1w7sdiMAUaJwq2RMwr7AtscXEdEsF";
        pub const SCOPE_PRICES: &str = "3NJYftD5sjVfxSnUdZ1wVML8f3aC6mp1CXCL6L7TnU8C";
        pub const SCOPE: ([u16; 4], [u16; 4]) = ([246, u16::MAX, u16::MAX, u16::MAX], [53, u16::MAX, u16::MAX, u16::MAX]);
        pub const PYTH_FEED: &str = "ff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace";
        pub const PYTH_PRICE: &str = "42amVS4KgzR9rA28tkVYqVXjq9Qa8dcZQMbH5EYFX6XC";
    }
}
