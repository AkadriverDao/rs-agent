use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;

pub type Approver = Arc<dyn Fn(&str, &Value) -> bool + Send + Sync>;

#[derive(Debug, Clone)]
pub enum PermissionLevel {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone)]
pub struct PermissionRule {
    pub pattern: String,
    pub level: PermissionLevel,
}

#[async_trait]
pub trait PermissionChecker: Send + Sync {
    async fn check(&self, tool_name: &str, _input: &Value) -> Result<(), String>;
}

pub struct DefaultPermissionChecker {
    allowed_tools: HashSet<String>,
    allowed_patterns: Vec<String>,
    ask_patterns: Vec<String>,
    rules: Vec<PermissionRule>,
    approver: Option<Approver>,
}

impl DefaultPermissionChecker {
    pub fn new() -> Self {
        Self {
            allowed_tools: HashSet::new(),
            allowed_patterns: Vec::new(),
            ask_patterns: Vec::new(),
            rules: Vec::new(),
            approver: None,
        }
    }

    pub fn allow_tool(mut self, name: &str) -> Self {
        self.allowed_tools.insert(name.to_string());
        self
    }

    pub fn allow_pattern(mut self, pattern: &str) -> Self {
        self.allowed_patterns.push(pattern.to_string());
        self
    }

    /// Set tools that require Ask permission (only checked when not already allowed)
    pub fn ask_pattern(mut self, pattern: &str) -> Self {
        self.ask_patterns.push(pattern.to_string());
        self
    }

    pub fn add_rule(mut self, rule: PermissionRule) -> Self {
        self.rules.push(rule);
        self
    }

    pub fn with_approver(mut self, approver: Approver) -> Self {
        self.approver = Some(approver);
        self
    }

    /// Configure from an agent definition's allow/ask lists
    pub fn from_agent(
        default_allowed: &[&str],
        ask_patterns: &[&str],
        approver: Option<Approver>,
    ) -> Self {
        let mut checker = Self::new();
        for p in default_allowed {
            if *p == "*" {
                // "*" means all tools allowed — set a catch-all pattern
                return checker.allow_pattern("*").with_approver_maybe(approver);
            }
            checker = checker.allow_tool(p);
        }
        for p in ask_patterns {
            checker = checker.ask_pattern(p);
        }
        checker.with_approver_maybe(approver)
    }

    fn with_approver_maybe(mut self, approver: Option<Approver>) -> Self {
        self.approver = approver;
        self
    }
}

impl Default for DefaultPermissionChecker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PermissionChecker for DefaultPermissionChecker {
    async fn check(&self, tool_name: &str, input: &Value) -> Result<(), String> {
        // 1. Check explicit tool allowlist
        if self.allowed_tools.contains(tool_name) {
            return Ok(());
        }

        // 2. Check wildcard allow patterns (e.g. "*")
        for pattern in &self.allowed_patterns {
            if wildcard_match(pattern, tool_name) {
                return Ok(());
            }
        }

        // 3. Check static rules
        for rule in &self.rules {
            if wildcard_match(&rule.pattern, tool_name) {
                match rule.level {
                    PermissionLevel::Allow => return Ok(()),
                    PermissionLevel::Deny => {
                        return Err(format!("Tool '{}' is denied by rule '{}'", tool_name, rule.pattern))
                    }
                    PermissionLevel::Ask => {
                        if let Some(approver) = &self.approver {
                            if approver(tool_name, input) {
                                return Ok(());
                            }
                        }
                        return Err(format!(
                            "Tool '{}' requires user permission (pattern: '{}')",
                            tool_name, rule.pattern
                        ))
                    }
                }
            }
        }

        // 4. Check ask_patterns — tools that need approval
        for pattern in &self.ask_patterns {
            if wildcard_match(pattern, tool_name) {
                if let Some(approver) = &self.approver {
                    if approver(tool_name, input) {
                        return Ok(());
                    }
                }
                return Err(format!(
                    "Tool '{}' requires user permission (pattern: '{}')",
                    tool_name, pattern
                ));
            }
        }

        Err(format!("Tool '{}' is not permitted", tool_name))
    }
}

fn wildcard_match(pattern: &str, name: &str) -> bool {
    let regex_pattern = format!("^{}$", pattern.replace('*', ".*").replace('?', "."));
    regex::Regex::new(&regex_pattern)
        .map(|re| re.is_match(name))
        .unwrap_or(false)
}
