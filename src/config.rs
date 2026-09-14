use serde::{Deserialize, Serialize};
use std::fmt;

use crate::error::{DaemonError, Result};

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

    pub fn parse(s: &str) -> Result<Self> {
        Self::from_str(s).ok_or_else(|| {
            DaemonError::InvalidInput(format!(
                "invalid tier '{s}': must be one of 'silver', 'gold', 'platinum'"
            ))
        })
    }
}

#[derive(Debug, Clone)]
pub struct TierConfig {
    #[allow(dead_code)]
    pub tier: Tier,
    pub xms: &'static str,
    pub xmx: &'static str,
    pub gc_args: &'static [&'static str],
    pub cgroup_memory_max: u64,
    pub cgroup_memory_swap_max: u64,
    pub cpu_quota_us: u64,
    pub cpu_period_us: u64,
}

impl TierConfig {
    pub fn for_tier(tier: Tier) -> Self {
        match tier {
            Tier::Silver => TierConfig {
                tier,
                xms: "256m",
                xmx: "1g",
                gc_args: &["-XX:+UseG1GC"],
                cgroup_memory_max: 1_342_177_280, // 1.25 GB
                cgroup_memory_swap_max: 0,
                cpu_quota_us: 100_000,             // 1 core
                cpu_period_us: 100_000,
            },
            Tier::Gold => TierConfig {
                tier,
                xms: "1g",
                xmx: "4g",
                gc_args: &["-XX:MaxGCPauseMillis=200", "-XX:+UseZGC"],
                cgroup_memory_max: 4_563_402_752, // 4.25 GB
                cgroup_memory_swap_max: 0,
                cpu_quota_us: 200_000,             // 2 cores
                cpu_period_us: 100_000,
            },
            Tier::Platinum => TierConfig {
                tier,
                xms: "4g",
                xmx: "32g",
                gc_args: &["-XX:+UseZGC", "-XX:+ZGenerational"],
                cgroup_memory_max: 34_628_173_824, // 32.25 GB
                cgroup_memory_swap_max: 0,
                cpu_quota_us: 400_000,             // 4 cores
                cpu_period_us: 100_000,
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

    pub fn cpu_max_string(&self) -> String {
        format!("{} {}", self.cpu_quota_us, self.cpu_period_us)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_from_str() {
        assert_eq!(Tier::from_str("silver"), Some(Tier::Silver));
        assert_eq!(Tier::from_str("GOLD"), Some(Tier::Gold));
        assert_eq!(Tier::from_str("Platinum"), Some(Tier::Platinum));
        assert_eq!(Tier::from_str("diamond"), None);
    }

    #[test]
    fn test_tier_parse() {
        assert!(Tier::parse("gold").is_ok());
        assert!(Tier::parse("invalid").is_err());
    }

    #[test]
    fn test_tier_jvm_args() {
        let silver = TierConfig::for_tier(Tier::Silver);
        assert_eq!(silver.tier, Tier::Silver);
        let args = silver.jvm_args();
        assert!(args.contains(&"-Xms256m".to_string()));
        assert!(args.contains(&"-Xmx1g".to_string()));
        assert!(args.contains(&"-XX:+UseG1GC".to_string()));

        let gold = TierConfig::for_tier(Tier::Gold);
        assert_eq!(gold.cgroup_memory_swap_max, 0);
        let gold_args = gold.jvm_args();
        assert!(gold_args.contains(&"-XX:+UseZGC".to_string()));
    }

    #[test]
    fn test_cpu_max_string() {
        let silver = TierConfig::for_tier(Tier::Silver);
        assert_eq!(silver.cpu_max_string(), "100000 100000");

        let gold = TierConfig::for_tier(Tier::Gold);
        assert_eq!(gold.cpu_max_string(), "200000 100000");

        let plat = TierConfig::for_tier(Tier::Platinum);
        assert_eq!(plat.cpu_max_string(), "400000 100000");
    }
}
