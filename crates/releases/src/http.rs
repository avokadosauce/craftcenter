//! The real transport: `ureq` with rustls, no cookies, no proxy auto-detection, no token.
//!
//! Two agents, because the two jobs want opposite redirect behaviour: the probe must *see* the
//! `302` that names the release tag, while fetching a manifest or an asset must follow GitHub's
//! redirect to its object store.

use std::io::Write;
use std::time::Duration;

use ureq::Agent;
use ureq::http::Response as HttpResponse;

use crate::{Error, Fetch, Response, USER_AGENT, read_capped};

/// Hosts this program is allowed to talk to. Anything else is a bug, and is refused here rather
/// than trusted to a caller: there is no setting that widens it.
const ALLOWED_HOSTS: &[&str] =
    &["github.com", "api.github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com", "raw.githubusercontent.com"];

/// A manifest is a couple of kilobytes and release metadata a few dozen; this ceiling keeps a
/// broken or hostile response from being buffered without bound.
const METADATA_CAP: usize = 4 * 1024 * 1024;

fn host_of(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit('@').next()?;
    let host = host.split(':').next()?;
    if host.is_empty() { None } else { Some(host) }
}

fn check_host(url: &str) -> Result<(), Error> {
    match host_of(url) {
        Some(host) if ALLOWED_HOSTS.contains(&host) => Ok(()),
        _ => Err(Error::Transport { url: url.to_owned(), message: "only github.com hosts are allowed, over https".to_owned() }),
    }
}

/// [`Fetch`] over the network.
pub struct Ureq {
    probe: Agent,
    follow: Agent,
}

impl Default for Ureq {
    fn default() -> Self {
        Self::new()
    }
}

impl Ureq {
    pub fn new() -> Self {
        let base = || {
            Agent::config_builder()
                .user_agent(USER_AGENT)
                .timeout_global(Some(Duration::from_secs(120)))
                // A 404 or a 302 is information this crate acts on, not a transport failure.
                .http_status_as_error(false)
        };
        let probe = base().max_redirects(0).max_redirects_will_error(false).build();
        let follow = base().max_redirects(10).build();
        Self { probe: probe.into(), follow: follow.into() }
    }

    fn agent(&self, follow_redirects: bool) -> &Agent {
        if follow_redirects { &self.follow } else { &self.probe }
    }
}

fn location_of<B>(response: &HttpResponse<B>) -> Option<String> {
    response.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_owned)
}

fn transport(url: &str, error: &ureq::Error) -> Error {
    Error::Transport { url: url.to_owned(), message: error.to_string() }
}

impl Fetch for Ureq {
    fn get(&self, url: &str, follow_redirects: bool) -> Result<Response, Error> {
        check_host(url)?;
        let mut response = self.agent(follow_redirects).get(url).call().map_err(|e| transport(url, &e))?;
        let status = response.status().as_u16();
        let location = location_of(&response);
        let body = read_capped(response.body_mut().as_reader(), METADATA_CAP).map_err(|source| Error::Io { path: url.to_owned(), source })?;
        Ok(Response { status, location, body })
    }

    fn get_to(&self, url: &str, sink: &mut dyn Write, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<u16, Error> {
        check_host(url)?;
        let mut response = self.follow.get(url).call().map_err(|e| transport(url, &e))?;
        let status = response.status().as_u16();
        if status != 200 {
            return Ok(status);
        }
        let total = response.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());

        let mut reader = response.body_mut().as_reader();
        let mut buffer = vec![0u8; 256 * 1024];
        let mut done: u64 = 0;
        loop {
            let read = std::io::Read::read(&mut reader, &mut buffer).map_err(|source| Error::Io { path: url.to_owned(), source })?;
            if read == 0 {
                break;
            }
            let chunk = buffer.get(..read).ok_or_else(|| Error::Transport { url: url.to_owned(), message: "short read".to_owned() })?;
            sink.write_all(chunk).map_err(|source| Error::Io { path: url.to_owned(), source })?;
            done = done.saturating_add(read as u64);
            progress(done, total);
        }
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_github_hosts_over_https_are_allowed() {
        assert!(check_host("https://github.com/o/r/releases/latest/download/SHA256SUMS.txt").is_ok());
        assert!(check_host("https://api.github.com/repos/o/r/releases/latest").is_ok());
        assert!(check_host("https://objects.githubusercontent.com/x").is_ok());
        assert!(check_host("http://github.com/o/r").is_err(), "plain http is refused");
        assert!(check_host("https://example.com/evil").is_err());
        // A redirect that tries to smuggle a host past the check.
        assert!(check_host("https://github.com.evil.test/x").is_err());
        assert!(check_host("https://evil.test/?x=github.com").is_err());
        assert!(check_host("https://user@evil.test/github.com").is_err());
    }

    #[test]
    fn the_user_agent_names_the_program_and_nothing_about_the_machine() {
        assert!(USER_AGENT.starts_with("craftcenter/"));
        assert!(!USER_AGENT.contains('@'));
    }
}
