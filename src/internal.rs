use crate::*;
use near_sdk::{near_bindgen, Balance, Promise};

pub use crate::types::*;
pub use crate::utils::*;

/****************************/
/* general Internal methods */
/****************************/
impl DiversifiedPool {
    /// Asserts that the method was called by the owner.
    pub fn assert_owner_calling(&self) {
        assert_eq!(
            &env::predecessor_account_id(),
            &self.owner_account_id,
            "Can only be called by the owner"
        )
    }
}

pub fn assert_min_amount(amount: u128) {
    assert!(amount >= FIVE_NEAR, "minimun amount is 5N");
}

/***************************************/
/* Internal methods staking-pool trait */
/***************************************/
#[near_bindgen]
impl DiversifiedPool {

    pub(crate) fn internal_deposit(&mut self) {

        let amount = env::attached_deposit();
        assert_min_amount(amount);

        let account_id = env::predecessor_account_id();
        let mut account = self.internal_get_account(&account_id);

        account.available += amount;
        self.total_available += amount;

        self.internal_update_account(&account_id, &account);

        env::log(
            format!(
                "@{} deposited {}. New available balance is {}",
                account_id, amount, account.available
            )
            .as_bytes(),
        );
    }

    //------------------------------
    pub(crate) fn internal_withdraw(&mut self, amount_requested: u128) {
        
        let account_id = env::predecessor_account_id();
        let mut acc = self.internal_get_account(&account_id);

        assert!(
            acc.available >= amount_requested,
            "Not enough available balance to withdraw the requested amount"
        );
        let to_withdraw = 
            if acc.available - amount_requested < ONE_NEAR_CENT/2  //small yotctos remain, withdraw all
                { acc.available } 
                else  { amount_requested };

        acc.available -= to_withdraw;
        assert!( !acc.is_empty() || acc.available >= self.min_account_balance,
            "The min balance for an open account is {} NEAR. You can remove all funds and close the account",
            self.min_account_balance/ONE_NEAR);

        self.internal_update_account(&account_id, &acc);

        self.total_available -= to_withdraw;
        Promise::new(account_id).transfer(to_withdraw);
    }


    //------------------------------
    pub(crate) fn internal_stake(&mut self, amount: Balance) {

        assert_min_amount(amount);

        let account_id = env::predecessor_account_id();
        let mut acc = self.internal_get_account(&account_id);

        assert!(
            acc.available >= amount,
            "Not enough available balance to stake the requested amount"
        );

        //use this operation to realize g-skash pending rewards
        acc.stake_realize_g_skash(self);
    
        // Calculate the number of "stake" shares that the account will receive for staking the given amount.
        let num_shares = self.stake_shares_from_amount(amount);
        assert!(num_shares > 0);

        //update user account
        acc.add_stake_shares(num_shares, amount);
        acc.available -= amount;
        //contract totals
        self.total_stake_shares += num_shares;
        self.total_for_staking += amount;
        assert!(self.total_available >= amount,"i_s_Inconsistency");
        self.total_available -= amount;

        //--SAVE ACCOUNT--
        self.internal_update_account(&account_id, &acc);

        //----------
        //check if the liquidity pool needs liquidity, and then use this opportunity to liquidate skash in the LP by internal-clearing 
        self.nslp_try_liquidate_skash_by_clearing();

    }

    //------------------------------
    pub(crate) fn internal_unstake(&mut self, amount_requested: u128) {

        let account_id = env::predecessor_account_id();
        let mut acc = self.internal_get_account(&account_id);

        let valued_shares = self.amount_from_stake_shares(acc.stake_shares);
        assert!(valued_shares >= amount_requested, "Not enough skash");

        //use this operation to realize g-skash pending rewards
        acc.stake_realize_g_skash(self);

        let remains_staked = valued_shares - amount_requested;
        //if less than one near would remain, unstake all
        let amount_to_unstake = if remains_staked > ONE_NEAR {
            amount_requested
        }
        else {
            valued_shares //unstake all
        };

        let num_shares: u128;
        //if unstake all staked near, we use all shares, so we include rewards in the unstaking...
        //when "unstaking_all" the amount unstaked is the requested amount PLUS ALL ACCUMULATED REWARDS
        if amount_to_unstake == valued_shares {
            num_shares = acc.stake_shares;
        } else {
            // Calculate the number of shares required to unstake the given amount.
            num_shares = self.stake_shares_from_amount(amount_to_unstake);
            assert!(num_shares > 0);
            assert!(
                acc.stake_shares >= num_shares,
                "Inconsistency. Not enough shares to unstake"
            );
        }

        //burn stake shares
        acc.sub_stake_shares(num_shares, amount_to_unstake);
        //the amount is now "unstaked"
        acc.unstaked += amount_to_unstake;
        acc.unstaked_requested_unlock_epoch = env::epoch_height() + self.internal_compute_current_unstaking_delay(amount_to_unstake); //when the unstake will be available
        //--contract totals
        self.total_stake_shares -= num_shares;
        self.total_for_staking -= amount_to_unstake;

        //--SAVE ACCOUNT--
        self.internal_update_account(&account_id, &acc);

        env::log(
            format!(
                "@{} unstaked {}. Has now {} unstaked and {} skash",
                account_id, amount_to_unstake, acc.unstaked, self.amount_from_stake_shares(acc.stake_shares)
            )
            .as_bytes(),
        );
        // env::log(
        //     format!(
        //         "Contract total staked balance is {}. Total number of shares {}",
        //         self.total_staked_balance, self.total_stake_shares
        //     )
        //     .as_bytes(),
        // );
    }

    //--------------------------------------------------
    /// computes unstaking delay on current situation
    pub fn internal_compute_current_unstaking_delay(&self, amount:u128) -> u64 {
        let mut normal_wait_staked_available:u128 =0;
        for (_,sp) in self.staking_pools.iter().enumerate() {
            //if the pool has no unstaking in process
            if !sp.busy_lock && sp.staked>0 && sp.unstaked==0 { 
                normal_wait_staked_available += sp.staked;
                if normal_wait_staked_available > amount {
                    return NUM_EPOCHS_TO_UNLOCK 
                }
            }
        }
        //all pools are in unstaking-delay, it will take double the time
        return 2 * NUM_EPOCHS_TO_UNLOCK; 
    }


    //--------------------------------
    pub(crate) fn add_amount_and_shares_preserve_share_price(
        &mut self,
        account_id: AccountId,
        amount: u128,
    ) {
        if amount > 0 {
            let num_shares = self.stake_shares_from_amount(amount);
            if num_shares > 0 {
                let account = &mut self.internal_get_account(&account_id);
                account.stake_shares += num_shares;
                &self.internal_update_account(&account_id, &account);
                // Increasing the total amount of "stake" shares.
                self.total_stake_shares += num_shares;
                self.total_for_staking += amount;
            }
        }
    }

    /// Returns the number of "stake" shares corresponding to the given near amount at current share_price
    /// if the amount & the shares are incorporated, price remains the same
    pub(crate) fn stake_shares_from_amount(&self, amount: Balance) -> u128 {
        return shares_from_amount(amount, self.total_for_staking, self.total_stake_shares);
    }

    /// Returns the amount corresponding to the given number of "stake" shares.
    pub(crate) fn amount_from_stake_shares(&self, num_shares: u128) -> u128 {
        return amount_from_shares(num_shares, self.total_for_staking, self.total_stake_shares);
    }

    //-----------------------------
    // NSLP: NEAR/SKASH Liquidity Pool
    //-----------------------------

    // NSLP shares are trickier to compute since the NSLP itself can have SKASH
    pub(crate) fn nslp_shares_from_amount(&self, amount: u128, nslp_account: &Account) -> u128 {
        let total_pool_value: u128 = nslp_account.available
            + self.amount_from_stake_shares(nslp_account.stake_shares)
            + nslp_account.unstaked;
        return shares_from_amount(amount, total_pool_value, nslp_account.nslp_shares);
    }

    // NSLP shares are trickier to compute since the NSLP itself can have SKASH
    pub(crate) fn amount_from_nslp_shares(&self, num_shares: u128, nslp_account: &Account) -> u128 {
        let total_pool_value: u128 = nslp_account.available
            + self.amount_from_stake_shares(nslp_account.stake_shares)
            + nslp_account.unstaked;
        return amount_from_shares(num_shares, total_pool_value, nslp_account.nslp_shares);
    }

    //----------------------------------
    // The LP acquires skash providing the sell-skash service
    // The LP needs to unstake the skash ASAP, to recover liquidity and to keep the fee low.
    // The LP can use staking orders to fast-liquidate its skash by clearing.
    // returns true if it uses the clearing to liquidate
    // ---------------------------------
    pub(crate) fn nslp_try_liquidate_skash_by_clearing(&mut self) -> bool {
        if self.total_for_staking <= self.total_actually_staked {
            //nothing ordered to be actually staked
            return false;
        }
        let amount_to_stake:u128 =  self.total_for_staking - self.total_actually_staked;
        let mut nslp_account = self.internal_get_nslp_account();
        if nslp_account.stake_shares > 0 {
            //how much skash does the nslp have?
            let valued_stake_shares = self.amount_from_stake_shares(nslp_account.stake_shares);
            //how much can we liquidate?
            let (shares_to_liquidate, amount_to_liquidate) =
                if amount_to_stake >= valued_stake_shares  { 
                    ( nslp_account.stake_shares, valued_stake_shares )
                } 
                else { 
                    ( self.stake_shares_from_amount(amount_to_stake), amount_to_stake )
                };
            //nslp sells-skash directly, contract now needs to stake less
            nslp_account.sub_stake_shares(shares_to_liquidate, amount_to_liquidate);
            self.total_stake_shares -= shares_to_liquidate;
            self.total_for_staking -= amount_to_liquidate; //nslp has burned shares, total_for_staking is less now
            self.total_available += amount_to_liquidate; // amount returns to total_available (since it was never staked to begin with)
            nslp_account.available += amount_to_liquidate; //nslp has more available now
            //save account
            self.internal_save_nslp_account(&nslp_account);
            return true;
        }        
        return false;
    }

    /// computes the disocunt_basis_points for NEAR/SKASH Swap based on NSLP Balance
    pub(crate) fn internal_get_discount_basis_points(
        &self,
        available_near: u128,
        max_nears_to_pay: u128,
    ) -> u16 {
        env::log(
            format!(
                "get_discount_basis_points available_near={}  max_nears_to_pay={}",
                available_near, max_nears_to_pay
            )
            .as_bytes(),
        );

        if available_near <= max_nears_to_pay {
            return self.nslp_max_discount_basis_points;
        }

        let near_after = available_near - max_nears_to_pay;

        if near_after < self.nslp_near_target / 20 {
            return self.nslp_max_discount_basis_points; // 1/20 (5%) target, discount capped at max%
        } 

        let discount_basis_plus_100 = self.nslp_near_target * 100 / near_after;
        if discount_basis_plus_100 <= 100 + u128::from(self.nslp_min_discount_basis_points) {
            return self.nslp_min_discount_basis_points; // target reached or surpassed
        } 

        let discount_basis_points = discount_basis_plus_100 - 100;
        if discount_basis_points > u128::from(self.nslp_max_discount_basis_points) {
            return self.nslp_max_discount_basis_points; //capped at max%
        } 

        return discount_basis_points as u16;
    }

    /// user method - NEAR/SKASH SWAP functions
    /// return how much NEAR you can get by selling x SKASH
    pub(crate) fn internal_get_near_amount_sell_skash(
        &self,
        available_near: u128,
        skash_to_sell: u128,
    ) -> u128 {
        let discount_basis_points =
            self.internal_get_discount_basis_points(available_near, skash_to_sell);
        assert!(discount_basis_points < 10000, "inconsistence d>1");
        let discount = apply_pct(discount_basis_points, skash_to_sell);
        return (skash_to_sell - discount).into(); //when SKASH is sold user gets a discounted value because the user skips the waiting period

        // env::log(
        //     format!(
        //         "@{} withdrawing {}. New unstaked balance is {}",
        //         account_id, amount, account.unstaked
        //     )
        //     .as_bytes(),
        // );
    }

    /// Inner method to get the given account or a new default value account.
    pub(crate) fn internal_get_account(&self, account_id: &String) -> Account {
        self.accounts.get(account_id).unwrap_or_default()
    }

    /// Inner method to save the given account for a given account ID.
    /// If the account balances are 0, the account is deleted instead to release storage.
    pub(crate) fn internal_update_account(&mut self, account_id: &String, account: &Account) {
        if account.is_empty() {
            self.accounts.remove(account_id);
        } else {
            self.accounts.insert(account_id, &account); //insert_or_update
        }
    }

    /// Inner method to get the given account or a new default value account.
    pub(crate) fn internal_get_nslp_account(&self) -> Account {
        self.accounts
            .get(&NSLP_INTERNAL_ACCOUNT.into())
            .unwrap_or_default()
    }
    pub(crate) fn internal_save_nslp_account(&mut self, nslp_account: &Account) {
        self.internal_update_account(&NSLP_INTERNAL_ACCOUNT.into(), &nslp_account);
    }


    /// finds a staking pool requiring some stake to get balanced
    /// WARN: (returns usize::MAX,0) if no pool requires staking/all are busy
    pub(crate) fn get_staking_pool_requiring_stake(&self, total_to_stake:u128) -> (usize,u128) {
        let mut selected_to_stake_amount: u128 = 0;
        let mut selected_sp_inx: usize = usize::MAX;

        for (sp_inx, sp) in self.staking_pools.iter().enumerate() {
            // if the pool is not busy, and this pool can stake
            if !sp.busy_lock && sp.weight_basis_points > 0 {
                // if this pool has an unbalance requiring staking
                let should_have = apply_pct(sp.weight_basis_points, self.total_for_staking);
                // this pool requires staking?
                if should_have > sp.staked {
                    // how much?
                    let require_amount = should_have - sp.staked;
                    // is this the most unbalanced pool so far?
                    if require_amount > selected_to_stake_amount {
                        selected_to_stake_amount = require_amount;
                        selected_sp_inx = sp_inx;
                    }
                }
            }
        }

        if selected_to_stake_amount>0 {
            //to avoid moving small amounts, if the remainder is less than 1K increase amount to include all in this movement
            if selected_to_stake_amount > total_to_stake { selected_to_stake_amount = total_to_stake };
            let remainder = total_to_stake - selected_to_stake_amount;
            if remainder <= MIN_STAKE_UNSTAKE_AMOUNT_MOVEMENT { 
                selected_to_stake_amount += remainder 
            };
        }

        return (selected_sp_inx, selected_to_stake_amount);
    }

    /// finds a staking pool requireing some stake to get balanced
    /// WARN: returns (usize::MAX,0) if no pool requires staking/all are busy
    pub(crate) fn get_staking_pool_requiring_unstake(&self, total_to_unstake:u128) -> (usize,u128) {
        let mut selected_to_unstake_amount: u128 = 0;
        let mut selected_stake: u128 = 0;
        let mut selected_sp_inx: usize = usize::MAX;

        for (sp_inx, sp) in self.staking_pools.iter().enumerate() {
            // if the pool is not busy, has stake, and has not unstaked blanace waiting for withdrawal
            if !sp.busy_lock && sp.staked > 0 && sp.unstaked == 0 {
                // if this pool has an unbalance requiring un-staking
                let should_have = apply_pct(sp.weight_basis_points, self.total_for_staking);
                // does this pool requires un-staking? (has too much staked?)
                if sp.staked > should_have {
                    // how much?
                    let unstake_amount = sp.staked - should_have;
                    // is this the most unbalanced pool so far?
                    if unstake_amount > selected_to_unstake_amount {
                        selected_to_unstake_amount = unstake_amount;
                        selected_stake = sp.staked;
                        selected_sp_inx = sp_inx;
                    }
                }
            }
        }

        if selected_to_unstake_amount>0 {
            if selected_to_unstake_amount > total_to_unstake { 
                selected_to_unstake_amount = total_to_unstake 
            };
            //to avoid moving small amounts, if the remainder is less than 1K and this pool can accomodate, increase amount
            let remainder = total_to_unstake - selected_to_unstake_amount;
            if remainder<=MIN_STAKE_UNSTAKE_AMOUNT_MOVEMENT && selected_stake>selected_to_unstake_amount+remainder+2*MIN_STAKE_UNSTAKE_AMOUNT_MOVEMENT { 
                selected_to_unstake_amount += remainder 
            };
        }
        return (selected_sp_inx, selected_to_unstake_amount);
    }

    // MULTI FUN TOKEN [NEP-138](https://github.com/near/NEPs/pull/138)
    /// Transfer `amount` of tok tokens from the caller of the contract (`predecessor_id`) to `receiver_id`.
    /// Requirements:
    /// * receiver_id must pre-exist
    pub fn internal_multifuntok_transfer(&mut self, sender_id: &AccountId, receiver_id: &AccountId, symbol:&String, am: u128) {
        let mut sender_acc = self.internal_get_account(&sender_id);
        let mut receiver_acc = self.internal_get_account(&receiver_id);
        match &symbol as &str {
            "NEAR" => {
                assert!(sender_acc.available >= am, "not enough NEAR at {}",sender_id);
                sender_acc.available -= am;
                receiver_acc.available += am;
            }
            "SKASH" => {
                let skash = self.amount_from_stake_shares(sender_acc.stake_shares);
                assert!(skash >= am,"not enough SKASH at {}",sender_id);
                let shares = self.stake_shares_from_amount(am);
                assert!(sender_acc.stake_shares <= shares,"IC");
                sender_acc.stake_shares -= shares;
                receiver_acc.stake_shares += shares;
            }
            "G-SKASH" => {
                sender_acc.stake_realize_g_skash(self);
                assert!(sender_acc.realized_g_skash >= am,"not enough G-SKASH at {}",sender_id);
                sender_acc.realized_g_skash -= am;
                receiver_acc.realized_g_skash += am;
            }
            _ => panic!("invalid symbol")
        }
        self.internal_update_account(&sender_id, &sender_acc);
        self.internal_update_account(&receiver_id, &receiver_acc);
    }

}
