//! A one-byte, domain-allocated contract discriminant and the high-order byte of
//! [`EventKey`](crate::store::EventKey). Each domain pins its own `const TAG`;
//! [`from_registrations`](crate::indexer::ContractIndexer::from_registrations)
//! enforces mutual uniqueness. Tag values are on-disk format: never reuse or
//! reorder.
//!
//! | Tag    | Domain contract        |
//! |--------|------------------------|
//! | `0x00` | PostageStamp           |
//! | `0x01` | StakeRegistry          |
//! | `0x02` | Redistribution         |
//! | `0x03` | Chequebook factory     |
//! | `0x04` | Swap price oracle      |
//! | `0x05` | Storage price oracle   |

use serde::{Deserialize, Serialize};
use vertex_storage::{DatabaseError, Decode, Encode};

/// A stable, one-byte contract discriminant. The wrapped byte is on-disk format;
/// see the module allocation table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContractTag(pub u8);

impl Encode for ContractTag {
    type Encoded = [u8; 1];

    fn encode(self) -> Self::Encoded {
        [self.0]
    }
}

impl Decode for ContractTag {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let [b]: [u8; 1] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self(b))
    }
}
