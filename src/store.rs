use std::{
    collections::VecDeque,
    net::SocketAddrV4,
    sync::{Arc, Mutex},
};

use rand::{RngExt, rngs::ThreadRng};

const BASE62: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

pub type SharedStore = Arc<Mutex<Store>>;

#[derive(Clone)]
pub struct StoredImage {
    pub png: Arc<[u8]>,
    pub thumbnail: Arc<image::RgbaImage>,
}

#[derive(Clone)]
pub struct Entry {
    pub id: String,
    pub image: StoredImage,
}

pub struct Store {
    entries: VecDeque<Entry>,
    max_images: usize,
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 8 && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

pub fn url(endpoint: SocketAddrV4, id: &str) -> String {
    format!("http://{endpoint}/image/{id}.png")
}

fn random_id(rng: &mut ThreadRng) -> String {
    (0..8)
        .map(|_| char::from(BASE62[rng.random_range(0..BASE62.len())]))
        .collect()
}

impl Store {
    pub fn new(max_images: usize) -> Self {
        assert!(max_images > 0);
        Self {
            entries: VecDeque::new(),
            max_images,
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<[u8]>> {
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| Arc::clone(&entry.image.png))
    }

    pub fn latest(&self) -> Option<Entry> {
        self.entries.back().cloned()
    }

    pub fn recent(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().rev()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn set_limit(&mut self, max_images: usize) {
        assert!(max_images > 0);
        self.max_images = max_images;
        self.evict();
    }

    fn evict(&mut self) {
        while self.entries.len() > self.max_images {
            self.entries.pop_front();
        }
    }

    // The caller keeps the store locked until publication succeeds or rolls back.
    pub fn publish<E>(
        &mut self,
        image: StoredImage,
        write: impl FnOnce(&str) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut rng = rand::rng();
        self.publish_with(image, || random_id(&mut rng), write)
    }

    fn publish_with<E>(
        &mut self,
        image: StoredImage,
        mut next_id: impl FnMut() -> String,
        write: impl FnOnce(&str) -> Result<(), E>,
    ) -> Result<(), E> {
        let id = loop {
            let candidate = next_id();
            if self.get(&candidate).is_none() {
                break candidate;
            }
        };
        self.entries.push_back(Entry {
            id: id.clone(),
            image,
        });
        if let Err(err) = write(&id) {
            self.entries.pop_back();
            return Err(err);
        }
        self.evict();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(value: u8) -> StoredImage {
        StoredImage {
            png: Arc::from([value]),
            thumbnail: Arc::new(image::RgbaImage::from_pixel(1, 1, image::Rgba([value; 4]))),
        }
    }

    #[test]
    fn distinct_valid_urls_for_identical_images() {
        let mut store = Store::new(100);
        let mut ids = std::collections::HashSet::new();
        let endpoint = "192.168.56.1:8888".parse().unwrap();
        for _ in 0..100 {
            store
                .publish(sample(1), |id| {
                    assert!(valid_id(id));
                    assert!(ids.insert(url(endpoint, id)));
                    Ok::<_, ()>(())
                })
                .unwrap();
        }
        assert_eq!(store.len(), 100);
        assert_eq!(
            url(endpoint, "k7Fp2xQ9"),
            "http://192.168.56.1:8888/image/k7Fp2xQ9.png"
        );
    }

    #[test]
    fn collisions_rollback_fifo_and_limit() {
        let mut store = Store::new(2);
        let mut first = ["abcdefgh", "abcdefgh", "Abcdefgh"].into_iter();
        for _ in 0..2 {
            store
                .publish_with(
                    sample(1),
                    || first.next().unwrap().to_owned(),
                    |_| Ok::<_, ()>(()),
                )
                .unwrap();
        }
        assert!(store.get("abcdefgh").is_some());
        assert_eq!(
            store.publish(sample(2), |_| Err("clipboard unavailable")),
            Err("clipboard unavailable")
        );
        assert_eq!(store.len(), 2);
        assert!(store.get("abcdefgh").is_some());

        store.publish(sample(3), |_| Ok::<_, ()>(())).unwrap();
        assert!(store.get("abcdefgh").is_none());
        assert!(store.get("Abcdefgh").is_some());
        store.set_limit(1);
        assert!(store.get("Abcdefgh").is_none());
        assert_eq!(&*store.latest().unwrap().image.png, &[3]);
        store.clear();
        assert!(store.is_empty());
        assert!(store.latest().is_none());
    }

    #[test]
    fn recent_images_are_newest_first_without_changing_fifo() {
        let mut store = Store::new(2);
        let mut ids = Vec::new();
        for value in 1..=2 {
            store
                .publish(sample(value), |id| {
                    ids.push(id.to_owned());
                    Ok::<_, ()>(())
                })
                .unwrap();
        }
        assert_eq!(
            store
                .recent()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            [&ids[1], &ids[0]]
        );
        assert_eq!(&*store.get(&ids[0]).unwrap(), &[1]);
        store.publish(sample(3), |_| Ok::<_, ()>(())).unwrap();
        assert!(store.get(&ids[0]).is_none());
        assert_eq!(
            store
                .recent()
                .last()
                .unwrap()
                .image
                .thumbnail
                .get_pixel(0, 0)
                .0,
            [2; 4]
        );
    }
}
