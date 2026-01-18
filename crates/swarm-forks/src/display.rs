use crate::{hardforks::Hardforks, ForkCondition};
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

/// A container to pretty-print a hardfork.
///
/// The fork is formatted as: `{name} <({eip})> @{condition}`
/// where the EIP part is optional.
#[derive(Debug)]
struct DisplayFork {
    /// The name of the hardfork (e.g. Frontier)
    name: String,
    /// The fork condition (timestamp)
    activated_at: ForkCondition,
    /// An optional EIP (e.g. `EIP-1`).
    eip: Option<String>,
}

impl core::fmt::Display for DisplayFork {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name_with_eip = if let Some(eip) = &self.eip {
            format!("{} ({})", self.name, eip)
        } else {
            self.name.clone()
        };

        match self.activated_at {
            ForkCondition::Timestamp(at) => {
                write!(f, "{name_with_eip:32} @{at}")?;
            }
            ForkCondition::Never => unreachable!(),
        }

        Ok(())
    }
}

/// A container for pretty-printing a list of hardforks.
///
/// An example of the output:
///
/// ```text
/// Hard forks (timestamp based):
/// - Frontier                         @1631112000
/// - FutureHardfork                   @1799999999
/// ```
#[derive(Debug)]
pub struct DisplayHardforks {
    /// A list of hardforks
    hardforks: Vec<DisplayFork>,
}

impl core::fmt::Display for DisplayHardforks {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "Hard forks (timestamp based):")?;
        for fork in &self.hardforks {
            writeln!(f, "- {fork}")?;
        }
        Ok(())
    }
}

impl DisplayHardforks {
    /// Creates a new [`DisplayHardforks`] from an iterator of hardforks.
    pub fn new<H: Hardforks>(hardforks: &H) -> Self {
        let mut hardforks_vec = Vec::new();

        for (fork, condition) in hardforks.forks_iter() {
            if condition != ForkCondition::Never {
                let display_fork = DisplayFork {
                    name: fork.name().to_string(),
                    activated_at: condition,
                    eip: None,
                };
                hardforks_vec.push(display_fork);
            }
        }

        Self {
            hardforks: hardforks_vec,
        }
    }
}
