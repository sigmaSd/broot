
use {
    super::*,
    crate::{
        content_search::*,
    },
    std::{
        fmt,
        path::Path,
    },
};

/// A pattern for searching in file content
#[derive(Debug, Clone)]
pub struct ContentExactPattern {
    needle: Needle,
}

impl fmt::Display for ContentExactPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl ContentExactPattern {

    pub fn from(pat: &str) -> Self {
        Self {
            needle: Needle::new(pat),
        }
    }

    pub fn as_str(&self) -> &str {
        self.needle.as_str()
    }

    pub fn is_empty(&self) -> bool {
        self.needle.is_empty()
    }

    pub fn to_regex_parts(&self) -> (String, String) {
        (self.as_str().to_string(), "".to_string())
    }

    pub fn score_of(&self, candidate: Candidate) -> Option<i32> {
        if !candidate.regular_file {
            return None;
        }
        match self.needle.search(&candidate.path) {
            Ok(ContentSearchResult::Found { .. }) => Some(1),
            Ok(ContentSearchResult::NotFound) => None,
            Ok(ContentSearchResult::NotSuitable) => {
                // debug!("{:?} isn't suitable for search", &candidate.path);
                None
            }
            Err(e) => {
                // today it mostly happens on empty files
                debug!("error while scanning {:?} : {:?}", &candidate.path, e);
                None
            }
        }
    }

    pub fn get_content_match(
        &self,
        path: &Path,
        desired_len: usize,
    ) -> Option<ContentMatch> {
        self.needle.get_match(path, desired_len)
    }
}

