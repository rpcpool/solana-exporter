# `solana_vote_account_balances`

## Description

Balances of the whitelisted vote accounts in lamports. Alpenglow (SIMD-0357) burns the Validator Admission Ticket from the vote account at each epoch boundary. A vote account below rent plus the VAT is left out of the next epoch, so alert well above that level. A vote account that does not exist reads as 0. The feed is empty when `vote_account_whitelist` is empty.

## Sample output

```
solana_vote_account_balances{pubkey="9GJmEHGom9eWo4np4L5vC6b6ri1Df2xN8KFoWixvD1Bs"} 11208000000
solana_vote_account_balances{pubkey="GREEDkpTvpKzcGvBu9qd36yk6BfjTWPShB67gLWuixMv"} 890000000
```
