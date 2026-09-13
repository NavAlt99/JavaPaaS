use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Tier {
    Silver,
    Gold,
    Platinum,
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tier::Silver => write!(f, "silver"),
            Tier::Gold => write!(f, "gold"),
            Tier::Platinum => write!(f, "platinum"),
        }
    }
}

impl Tier {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "silver" => Some(Tier::Silver),
            "gold" => Some(Tier::Gold),
            "platinum" => Some(Tier::Platinum),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TierConfig {
    pub tier: Tier,
    pub xms: &'static str,
    pub xmx: &'static str,
    pub gc_args: &'static [&'static str],
    pub cgroup_memory_max: u64,
    pub cgroup_memory_swap_max: u64,
}

impl TierConfig {
    pub fn for_tier(tier: Tier) -> Self {
        match tier {
            Tier::Silver => TierConfig {
                tier,
                xms: "256m",
                xmx: "1g",
                gc_args: &["-XX:+UseG1GC"],
                cgroup_memory_max: 1_342_177_280,
                cgroup_memory_swap_max: 0,
            },
            Tier::Gold => TierConfig {
                tier,
                xms: "1g",
                xmx: "4g",
                gc_args: &["-XX:MaxGCPauseMillis=200", "-XX:+UseZGC"],
                cgroup_memory_max: 4_563_402_752,
                cgroup_memory_swap_max: 0,
            },
            Tier::Platinum => TierConfig {
                tier,
                xms: "4g",
                xmx: "32g",
                gc_args: &["-XX:+UseZGC", "-XX:+ZGenerational"],
                cgroup_memory_max: 34_628_173_824,
                cgroup_memory_swap_max: 0,
            },
        }
    }

    pub fn jvm_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        args.push(format!("-Xms{}", self.xms));
        args.push(format!("-Xmx{}", self.xmx));
        args.extend(self.gc_args.iter().map(|s| s.to_string()));
        args
    }
}
