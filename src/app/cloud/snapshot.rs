use std::collections::{HashMap, HashSet};

use rustman_core::secrets::{CREDENTIAL_HEADERS, is_secret_value, strip_secrets};
use rustman_core::store::Workspace;
use serde::{Deserialize, Serialize};

use crate::domain::{
    collection::{Collection, SavedRequest},
    group::Group,
};

use super::s3::{LIMIT, digest};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Snapshot {
    version: u16,
    collections: Vec<Collection>,
    groups: Vec<Group>,
    requests: Vec<SavedRequest>,
}

impl Snapshot {
    pub(super) fn from_local(workspace: Workspace) -> Self {
        Self {
            version: 1,
            collections: workspace.collections,
            groups: workspace.groups,
            requests: workspace.requests,
        }
        .normalize()
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, String> {
        let snapshot: Self = serde_json::from_slice(bytes)
            .map_err(|_| "The cloud object isn't a Rustman workspace".to_owned())?;
        snapshot.validate()?;
        Ok(snapshot.normalize())
    }

    fn normalize(mut self) -> Self {
        self.collections.sort_by(|a, b| a.id.cmp(&b.id));
        self.groups.sort_by(|a, b| a.id.cmp(&b.id));
        self.requests.sort_by(|a, b| a.id.cmp(&b.id));
        for request in &mut self.requests {
            strip_secrets(request);
            if let Some(file) = &mut request.binary_file {
                file.data = None;
            }
            for field in &mut request.form_data_fields {
                field.file_data = None;
            }
        }
        self
    }

    pub(super) fn encode(&self) -> Result<Vec<u8>, String> {
        let bytes =
            serde_json::to_vec(self).map_err(|_| "Couldn't encode the workspace".to_owned())?;
        if bytes.len() > LIMIT {
            return Err("The workspace exceeds the 32 MiB cloud limit".to_owned());
        }
        Ok(bytes)
    }

    pub(super) fn fingerprint(&self) -> Result<String, String> {
        self.encode().map(|bytes| digest(&bytes))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.collections.is_empty()
    }

    pub(super) fn count(&self) -> usize {
        self.collections.len()
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("This cloud workspace was written in an unsupported format".to_owned());
        }
        let invalid = || {
            "The cloud workspace has missing or duplicate collection, folder or request IDs"
                .to_owned()
        };
        let mut ids = HashSet::new();
        for id in self
            .collections
            .iter()
            .map(|c| c.id.as_str())
            .chain(self.groups.iter().map(|g| g.id.as_str()))
            .chain(self.requests.iter().map(|r| r.id.as_str()))
        {
            if id.trim().is_empty() || !ids.insert(id) {
                return Err(invalid());
            }
        }
        let collections: HashSet<_> = self.collections.iter().map(|c| c.id.as_str()).collect();
        let groups: HashMap<_, _> = self.groups.iter().map(|g| (g.id.as_str(), g)).collect();
        let mut owners = HashMap::new();
        for group in &self.groups {
            let mut seen = HashSet::new();
            let mut parent = group;
            let owner = loop {
                if let Some(owner) = owners.get(parent.id.as_str()) {
                    break *owner;
                }
                if !seen.insert(parent.id.as_str()) {
                    return Err(invalid());
                }
                if parent.parent_is_collection {
                    break collections
                        .get(parent.parent_id.as_str())
                        .copied()
                        .ok_or_else(invalid)?;
                }
                parent = groups.get(parent.parent_id.as_str()).ok_or_else(invalid)?;
            };
            owners.extend(seen.into_iter().map(|id| (id, owner)));
        }
        for request in &self.requests {
            if !collections.contains(request.collection_id.as_str())
                || request
                    .group_id
                    .as_deref()
                    .is_some_and(|id| owners.get(id) != Some(&request.collection_id.as_str()))
            {
                return Err(invalid());
            }
        }
        Ok(())
    }

    pub(super) fn into_workspace(self, local: Workspace) -> Workspace {
        let by_id: HashMap<_, _> = local.requests.iter().map(|r| (r.id.as_str(), r)).collect();
        let mut requests = self.requests;
        for request in &mut requests {
            if let Some(old) = by_id.get(request.id.as_str()) {
                keep_local(request, old);
            }
        }
        let mut collections = self.collections;
        let mut groups = self.groups;
        collections.sort_by_key(|c| c.created_at);
        groups.sort_by_key(|g| g.sort_order);
        requests.sort_by_key(|r| r.sort_order);
        Workspace {
            collections,
            groups,
            requests,
            environments: local.environments,
        }
    }
}

fn keep_local(request: &mut SavedRequest, old: &SavedRequest) {
    fn fill(value: &mut String, local: &str) {
        if value.is_empty() && is_secret_value(local) {
            local.clone_into(value);
        }
    }
    fill(&mut request.bearer_token, &old.bearer_token);
    fill(&mut request.basic_pass, &old.basic_pass);
    fill(&mut request.api_key_value, &old.api_key_value);
    fill(&mut request.jwt_secret, &old.jwt_secret);
    fill(&mut request.cookie_string, &old.cookie_string);
    let headers = request
        .headers
        .iter_mut()
        .filter(|h| {
            CREDENTIAL_HEADERS
                .iter()
                .any(|name| name.eq_ignore_ascii_case(h.key.trim()))
        })
        .zip(std::iter::repeat(&old.headers));
    let cookies = request
        .cookies
        .iter_mut()
        .zip(std::iter::repeat(&old.cookies));
    for (row, rows) in headers.chain(cookies) {
        if let Some(local) = rows
            .iter()
            .find(|local| local.id == row.id && local.key.eq_ignore_ascii_case(&row.key))
        {
            fill(&mut row.value, &local.value);
        }
    }
    if let (Some(file), Some(old)) = (&mut request.binary_file, &old.binary_file)
        && file.name == old.name
    {
        file.data = old.data.clone();
    }
    for field in &mut request.form_data_fields {
        if let Some(old) = old.form_data_fields.iter().find(|old| {
            old.id == field.id
                && old.field_type == field.field_type
                && old.file_name == field.file_name
        }) {
            field.file_data.clone_from(&old.file_data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::request::{BinaryFile, BodyType};

    #[test]
    fn imports_a_snapshot_and_preserves_device_credentials_and_file() {
        let collection = Collection::new("Example".into());
        let mut request = SavedRequest::new_in(collection.id.clone(), "Sample".into());
        request.bearer_token = "local-token".into();
        request.body_type = BodyType::Binary;
        request.binary_file = Some(BinaryFile {
            name: "payload.bin".into(),
            data: Some("YQ==".into()),
        });
        let device = Workspace {
            collections: vec![collection],
            requests: vec![request],
            ..Workspace::default()
        };

        let snapshot = Snapshot::from_local(device.clone());
        let bytes = snapshot.encode().unwrap();
        let decoded = Snapshot::decode(&bytes).unwrap();
        assert!(decoded.requests[0].bearer_token.is_empty());
        assert!(
            decoded.requests[0]
                .binary_file
                .as_ref()
                .unwrap()
                .data
                .is_none()
        );
        let restored = decoded.into_workspace(device);
        assert_eq!(restored.requests[0].bearer_token, "local-token");
        assert_eq!(
            restored.requests[0]
                .binary_file
                .as_ref()
                .unwrap()
                .data
                .as_deref(),
            Some("YQ==")
        );
    }
}
