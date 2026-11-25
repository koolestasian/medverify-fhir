//! USPS codes for jurisdictions whose rules MedVerify applies.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

macro_rules! states {
    ($($code:ident),+ $(,)?) => {
        /// A state, the District of Columbia or an inhabited U.S. territory, by its uppercase USPS
        /// code. Parsing is exact: `"tx"`, `" TX"` and `""` are rejected rather than matching nothing.
        #[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        pub enum State {
            $($code),+
        }

        impl State {
            pub const ALL: &'static [State] = &[$(State::$code),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(State::$code => stringify!($code)),+
                }
            }
        }

        impl FromStr for State {
            type Err = UnknownState;

            fn from_str(code: &str) -> Result<Self, Self::Err> {
                match code {
                    $(stringify!($code) => Ok(State::$code),)+
                    _ => Err(UnknownState),
                }
            }
        }
    };
}

states!(
    AL, AK, AZ, AR, CA, CO, CT, DE, DC, FL, GA, HI, ID, IL, IN, IA, KS, KY, LA, ME, MD, MA, MI, MN,
    MS, MO, MT, NE, NV, NH, NJ, NM, NY, NC, ND, OH, OK, OR, PA, RI, SC, SD, TN, TX, UT, VT, VA, WA,
    WV, WI, WY, AS, GU, MP, PR, VI,
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownState;

impl fmt::Display for UnknownState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not an uppercase USPS state or territory code")
    }
}

impl std::error::Error for UnknownState {}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing_is_exact_and_round_trips() {
        assert_eq!(State::ALL.len(), 56);
        for state in State::ALL {
            assert_eq!(state.as_str().parse(), Ok(*state));
            assert_eq!(
                serde_json::to_string(state).unwrap(),
                format!("\"{state}\"")
            );
        }
        for bad in ["tx", "Tx", " TX", "TX ", "", "TXX", "ZZ", "AA"] {
            assert_eq!(bad.parse::<State>(), Err(UnknownState), "{bad:?}");
            assert!(serde_json::from_str::<State>(&format!("{bad:?}")).is_err());
        }
    }
}
