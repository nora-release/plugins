use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const COOLDOWN: Duration = Duration::from_secs(10 * 60);
const CAPACITY: usize = 512;

#[derive(Default)]
struct Host {
    refused_until: Option<Instant>,
    too_big: Option<(usize, Instant)>,
}

fn hosts() -> &'static Mutex<BTreeMap<String, Host>> {
    static HOSTS: OnceLock<Mutex<BTreeMap<String, Host>>> = OnceLock::new();
    HOSTS.get_or_init(Default::default)
}

pub(super) fn blocked(host: &str, size: usize) -> bool {
    let mut hosts = hosts().lock().unwrap_or_else(|error| error.into_inner());
    expire(&mut hosts);
    hosts.get(host).is_some_and(|host| {
        host.refused_until.is_some() || host.too_big.is_some_and(|(limit, _)| size >= limit)
    })
}

pub(super) fn refused(host: &str) {
    update(host, |host| {
        host.refused_until = Some(Instant::now() + COOLDOWN);
    });
}

pub(super) fn too_big(host: &str, size: usize) {
    update(host, |host| {
        let size = host.too_big.map_or(size, |(limit, _)| limit.min(size));
        host.too_big = Some((size, Instant::now() + COOLDOWN));
    });
}

fn update(host: &str, update: impl FnOnce(&mut Host)) {
    let mut hosts = hosts().lock().unwrap_or_else(|error| error.into_inner());
    expire(&mut hosts);
    if hosts.len() >= CAPACITY && !hosts.contains_key(host) {
        let oldest = hosts
            .iter()
            .min_by_key(|(_, host)| {
                host.refused_until
                    .into_iter()
                    .chain(host.too_big.map(|(_, until)| until))
                    .max()
            })
            .map(|(host, _)| host.clone());
        if let Some(oldest) = oldest {
            hosts.remove(&oldest);
        }
    }
    update(hosts.entry(host.to_owned()).or_default());
}

fn expire(hosts: &mut BTreeMap<String, Host>) {
    let now = Instant::now();
    hosts.retain(|_, host| {
        host.refused_until = host.refused_until.filter(|until| *until > now);
        host.too_big = host.too_big.filter(|(_, until)| *until > now);
        host.refused_until.is_some() || host.too_big.is_some()
    });
}
