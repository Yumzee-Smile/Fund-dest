# TA register - class USD-D

As of 2026-10-07T18:00:00Z. Simulated data; the legal register of record stays with the TA.

## Reconciliation

- registrar total_shares 374,360.9527849 | sum of SAC balances 374,360.9527849 | journal 374,360.9527849 -> RECONCILED
- vault cash 0.0000000 | pending + claimable 0.0000000 -> RECONCILED

## Holders

| Investor | Name | Shares | Locked | Jurisdiction | KYC expiry |
|---|---|---|---|---|---|
| inv_01 | Amélie Laurent | 25,000.0000000 | 0.0000000 | FR | 2027-06-30T00:00:00Z |
| inv_02 | Jonas Weber | 3,000.0084799 | 0.0000000 | DE | 2027-03-15T00:00:00Z |
| inv_03 | Pieter van Dijk | 40,000.0000000 | 0.0000000 | NL | 2027-11-02T00:00:00Z |
| inv_04 | Lucía Fernández | 2,000.0000000 | 0.0000000 | ES | 2028-01-19T00:00:00Z |
| inv_05 | Marco Bianchi | 0.0000000 | 0.0000000 | IT | 2027-09-09T00:00:00Z |
| inv_05b | Marco Bianchi (replacement wallet) | 12,000.0000000 | 0.0000000 | IT | 2027-09-09T00:00:00Z |
| inv_06 | Helvetia Reserve AG | 134,999.8970004 | 0.0000000 | CH | 2027-12-31T00:00:00Z |
| inv_07 | Siobhán Murphy | 7,500.0000000 | 0.0000000 | IE | 2026-10-06T12:00:00Z (EXPIRED) |
| inv_08 | Marie Dubois | 750.0000000 | 0.0000000 | FR | 2027-04-04T00:00:00Z |
| inv_09 | Tiago Almeida | 11,333.3333333 | 0.0000000 | PT | 2027-08-15T00:00:00Z |
| inv_10 | Bramwell Treasury SARL | 0.0000000 | 0.0000000 | LU | 2028-03-31T00:00:00Z |
| inv_11 | Katharina Gruber | 15,000.0000000 | 0.0000000 | AT | 2027-02-28T00:00:00Z |
| inv_12 | Noor Al Mansoori | 0.0000000 | 0.0000000 | AE | 2026-10-07T00:00:00Z (EXPIRED) |
| inv_13 | Marie Dubois | 0.0000000 | 0.0000000 | BE | 2027-10-10T00:00:00Z |
| inv_14 | Lim Wei Ling | 20,999.5000000 | 0.0000000 | SG | 2027-05-05T00:00:00Z |
| inv_15 | Sophie Janssens | 9,000.5000000 | 0.0000000 | BE | 2027-07-07T00:00:00Z |
| inv_18 | Giulia Rossi | 5,000.0000000 | 0.0000000 | IT | 2027-03-03T00:00:00Z |
| inv_19 | Oliver Schmid | 60,000.0000000 | 0.0000000 | CH | 2026-10-06T20:00:00Z (EXPIRED) |
| inv_20 | Ana Costa | 22,777.7183113 | 0.0000000 | PT | 2027-06-06T00:00:00Z |
| inv_23 | Robert Miller | 0.0000000 | 0.0000000 | US | 2027-08-08T00:00:00Z |
| inv_25 | Lukas Novak | 4,999.9956600 | 0.0000000 | AT | 2027-02-14T00:00:00Z |

## KYC expiring or expired

- inv_07: 2026-10-06T12:00:00Z (expired 1 day(s) ago)
- inv_12: 2026-10-07T00:00:00Z (expired <1 day ago)
- inv_19: 2026-10-06T20:00:00Z (expired <1 day ago)

## Exceptions for a person to act on

- 2026-10-05T08:15:40Z subscribe inv_02: BelowMinimum
- 2026-10-05T12:02:47Z subscribe inv_23: JurisdictionBlocked
- 2026-10-06T13:05:00Z cancel inv_06: CutoffPassed
- 2026-10-07T08:45:00Z redeem inv_12: CashAddressNotAllowed
- 2026-10-07T09:30:00Z transfer inv_11: KycExpired
- 2026-10-07T09:45:00Z transfer inv_19: KycExpired
- 2026-10-07T10:02:00Z distribution.claim inv_01: CashAddressNotAllowed
- 2026-10-07T10:15:00Z forced_transfer inv_05: InsufficientAdmin
- 2026-10-07T13:10:00Z strike : StalePrice
- 2026-10-07T14:05:00Z strike : NavMoveTooLarge
