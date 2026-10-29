# `solana_vote_account_authority`

## Description

The authorities of each whitelisted vote account, read from the account data. Each series has the value 1. The `role` label is one of these:

- `withdrawer`: the authorized withdrawer. It can withdraw the balance and change all the other authorities.
- `voter`: an authorized voter. A voter change takes effect at a later epoch. Until then, the account shows the current voter and the scheduled voter.
- `inflation_rewards_collector` and `block_revenue_collector`: the reward recipients. Only vote state V4 has these fields.

The feed resets at each update, so a replaced authority does not stay as an old series. To detect a change, alert when the `authority` label is different from the expected key. The feed is empty when `vote_account_whitelist` is empty.

## Sample output

```
solana_vote_account_authority{authority="<withdrawer>",pubkey="<vote account>",role="withdrawer"} 1
solana_vote_account_authority{authority="<node identity>",pubkey="<vote account>",role="voter"} 1
solana_vote_account_authority{authority="<vote account>",pubkey="<vote account>",role="inflation_rewards_collector"} 1
solana_vote_account_authority{authority="<node identity>",pubkey="<vote account>",role="block_revenue_collector"} 1
```
