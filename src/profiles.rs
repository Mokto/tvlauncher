use crate::data::{write_private, DIR};
use serde_json::{json, Value};
use std::io;

pub const AVATAR_COUNT: usize = 6;
pub const NEW_PIN_LEN: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Manager,
    Adult,
    Kid,
}

impl Role {
    pub const ALL: [Self; 3] = [Self::Manager, Self::Adult, Self::Kid];

    pub fn label(self) -> &'static str {
        match self {
            Self::Manager => "Manager",
            Self::Adult => "Adult",
            Self::Kid => "Kid",
        }
    }
}

#[derive(Clone)]
pub struct Profile {
    pub name: String,
    pub avatar: usize,
    pub role: Role,
    pub pin: String,
    pub apps: Vec<String>,
}

impl Profile {
    pub fn new(name: String, role: Role) -> Self {
        Self {
            name,
            avatar: 0,
            role,
            pin: String::new(),
            apps: vec![],
        }
    }

    pub fn allows(&self, app: &str) -> bool {
        self.role != Role::Kid || self.apps.iter().any(|id| id == app)
    }
}

pub fn valid_pin(pin: &str) -> bool {
    pin.is_empty() || ((4..=8).contains(&pin.len()) && pin.bytes().all(|b| b.is_ascii_digit()))
}

pub fn valid_name(name: &str) -> bool {
    (1..=20).contains(&name.chars().count()) && !name.chars().any(char::is_control)
}

pub fn load(root: &str) -> io::Result<Vec<Profile>> {
    let bytes = match std::fs::read(format!("{root}{DIR}/profiles.json")) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![Profile::new("Owner".into(), Role::Manager)]),
        Err(e) => return Err(e),
    };
    let v: Value = serde_json::from_slice(&bytes)?;
    let parse = || {
        let list: Vec<_> = v
            .as_array()?
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let name = p["name"].as_str()?.to_string();
                let role = Role::ALL.into_iter().find(|r| p["role"].as_str() == Some(r.label()))?;
                let pin = p["pin"].as_str()?.to_string();
                let apps: Vec<String> = serde_json::from_value(p["apps"].clone()).ok()?;
                let avatar = p["avatar"]
                    .as_u64()
                    .and_then(|id| usize::try_from(id).ok())
                    .filter(|&id| id < AVATAR_COUNT)
                    .unwrap_or(i % AVATAR_COUNT);
                (valid_name(&name) && valid_pin(&pin)).then_some(Profile {
                    name,
                    role,
                    pin,
                    apps,
                    avatar,
                })
            })
            .collect::<Option<_>>()?;
        (list.first()?.role == Role::Manager).then_some(list)
    };
    parse().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid profiles.json"))
}

pub fn save(root: &str, list: &[Profile]) -> io::Result<()> {
    std::fs::create_dir_all(format!("{root}{DIR}"))?;
    let list: Vec<_> = list
        .iter()
        .map(|p| json!({"name": p.name, "role": p.role.label(), "pin": p.pin, "apps": p.apps, "avatar": p.avatar}))
        .collect();
    write_private(&format!("{root}{DIR}/profiles.json"), &serde_json::to_vec(&list)?)
}

pub fn data_root(root: &str, index: usize) -> String {
    if index == 0 {
        root.into()
    } else {
        format!("{root}{DIR}/profiles/{index}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn legacy_owner_and_isolated_restricted_profile() {
        let dir = std::env::temp_dir().join(format!("tvlauncher-profiles-{}", std::process::id()));
        let root = dir.to_str().unwrap();
        let mut list = load(root).unwrap();
        assert_eq!(data_root(root, 0), root);
        assert!(list[0].allows("any"));
        let mut kid = Profile::new("Kids".into(), Role::Kid);
        kid.apps.push("allowed".into());
        kid.pin = "0123".into();
        kid.avatar = 4;
        list.push(kid);
        save(root, &list).unwrap();
        let list = load(root).unwrap();
        assert!(!list[1].allows("new"));
        assert!(list[1].allows("allowed"));
        assert_eq!(list[1].pin, "0123");
        assert_eq!(list[1].avatar, 4);
        assert_ne!(data_root(root, 0), data_root(root, 1));
        let path = format!("{root}{DIR}/profiles.json");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::write(path, "[]").unwrap();
        assert!(load(root).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
