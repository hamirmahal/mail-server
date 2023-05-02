use roaring::RoaringBitmap;

use crate::{
    fts::{builder::MAX_TOKEN_LENGTH, stemmer::Stemmer, tokenizers::Tokenizer},
    BitmapKey, ReadTransaction, ValueKey, HASH_EXACT, HASH_STEMMED,
};

use super::{term_index::TermIndex, Language};

impl ReadTransaction<'_> {
    #[maybe_async::maybe_async]
    pub(crate) async fn fts_query(
        &mut self,
        account_id: u32,
        collection: u8,
        field: u8,
        text: &str,
        language: Language,
        match_phrase: bool,
    ) -> crate::Result<Option<RoaringBitmap>> {
        if match_phrase {
            let mut phrase = Vec::new();
            let mut bit_keys = Vec::new();
            for token in Tokenizer::new(text, language, MAX_TOKEN_LENGTH) {
                let key = BitmapKey::hash(
                    token.word.as_ref(),
                    account_id,
                    collection,
                    HASH_EXACT,
                    field,
                );
                if !bit_keys.contains(&key) {
                    bit_keys.push(key);
                }

                phrase.push(token.word);
            }
            let bitmaps = match self.get_bitmaps_intersection(bit_keys).await? {
                Some(b) if !b.is_empty() => b,
                _ => return Ok(None),
            };

            match phrase.len() {
                0 => return Ok(None),
                1 => return Ok(Some(bitmaps)),
                _ => (),
            }

            let mut results = RoaringBitmap::new();
            for document_id in bitmaps {
                self.refresh_if_old().await?;
                if let Some(term_index) = self
                    .get_value::<TermIndex>(ValueKey::term_index(
                        account_id,
                        collection,
                        document_id,
                    ))
                    .await?
                {
                    if term_index
                        .match_terms(
                            &phrase
                                .iter()
                                .map(|w| term_index.get_match_term(w, None))
                                .collect::<Vec<_>>(),
                            field.into(),
                            true,
                            false,
                            false,
                        )
                        .map_err(|e| {
                            crate::Error::InternalError(format!(
                                "TermIndex match_terms failed for {account_id}/{collection}/{document_id}: {e:?}"
                            ))
                        })?
                        .is_some()
                    {
                        results.insert(document_id);
                    }
                } else {
                    tracing::debug!(
                        event = "error",
                        context = "fts_query",
                        account_id = account_id,
                        collection = collection,
                        document_id = document_id,
                        "Document is missing a term index",
                    );
                }
            }

            if !results.is_empty() {
                Ok(Some(results))
            } else {
                Ok(None)
            }
        } else {
            let mut bitmaps = RoaringBitmap::new();

            for token in Stemmer::new(text, language, MAX_TOKEN_LENGTH) {
                let token1 =
                    BitmapKey::hash(&token.word, account_id, collection, HASH_EXACT, field);
                let token2 = if let Some(stemmed_word) = token.stemmed_word {
                    BitmapKey::hash(&stemmed_word, account_id, collection, HASH_STEMMED, field)
                } else {
                    let mut token2 = token1.clone();
                    token2.family &= !HASH_EXACT;
                    token2.family |= HASH_STEMMED;
                    token2
                };

                self.refresh_if_old().await?;

                match self.get_bitmaps_union(vec![token1, token2]).await? {
                    Some(b) if !b.is_empty() => {
                        if !bitmaps.is_empty() {
                            bitmaps &= b;
                            if bitmaps.is_empty() {
                                return Ok(None);
                            }
                        } else {
                            bitmaps = b;
                        }
                    }
                    _ => return Ok(None),
                };
            }

            Ok(Some(bitmaps))
        }
    }
}
