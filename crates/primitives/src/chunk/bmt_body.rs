use swarm_primitives_traits::{
    ChunkAddress, ChunkBody, ChunkDecoding, ChunkEncoding, Span, CHUNK_SIZE, SEGMENT_SIZE,
    SPAN_SIZE,
};

use crate::bmt::{Hasher, HasherBuilder};

#[derive(Debug, thiserror::Error)]
pub enum BMTBodyError {
    #[error("Data size exceeds the maximum allowed ({max_size} bytes), got {actual_size} bytes")]
    DataTooLarge { max_size: usize, actual_size: usize },
    #[error("Data too small ({min_size} bytes), got {actual_size} bytes")]
    InsufficientData { min_size: usize, actual_size: usize },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct BMTBody {
    span: Span,
    data: Vec<u8>,
}

impl BMTBody {
    pub fn new(span: Span, data: Vec<u8>) -> Result<Self, BMTBodyError> {
        match data.len() <= CHUNK_SIZE {
            true => Ok(Self { span, data }),
            false => Err(BMTBodyError::DataTooLarge {
                max_size: CHUNK_SIZE,
                actual_size: data.len(),
            }),
        }
    }
}

impl ChunkBody for BMTBody {
    fn hash(&self) -> ChunkAddress {
        // TODO: Implement BMT hasher pooling from a global static
        let mut hasher: Hasher = HasherBuilder::default().build().unwrap();
        hasher.set_span(self.span);
        hasher.write(&self.data);

        let mut result = [0u8; SEGMENT_SIZE];
        hasher.hash(&mut result);

        result.into()
    }
}

impl ChunkEncoding for BMTBody {
    fn size(&self) -> usize {
        SPAN_SIZE + self.data.len()
    }

    fn to_boxed_slice(&self) -> Box<[u8]> {
        let mut result = Vec::with_capacity(self.size());
        result.extend_from_slice(&self.span.to_le_bytes());
        result.extend_from_slice(&self.data);

        result.into_boxed_slice()
    }
}

impl ChunkDecoding for BMTBody {
    #[allow(refining_impl_trait)]
    fn from_slice(buf: &[u8]) -> Result<Self, BMTBodyError> {
        if buf.len() < SPAN_SIZE {
            return Err(BMTBodyError::InsufficientData {
                min_size: SPAN_SIZE,
                actual_size: buf.len(),
            });
        }

        // SAFETY: Unwrap is safe as indexing of the slice is guarded by the above conditional.
        let span = Span::from_le_bytes(buf[0..SPAN_SIZE].try_into().unwrap());
        // SAFETY: Guard for the condition whereby the data length of the BMT body is zero (raw
        // data consists only of the span).
        let data = match buf.len() > SPAN_SIZE {
            true => buf[SPAN_SIZE..].to_vec(),
            false => vec![],
        };

        Ok(BMTBody::new(span, data)?)
    }
}
