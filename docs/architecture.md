# Architecture

## Overview

- Integrity -> chunk address checking
- Authorisation -> stamping (e.g. economic authorisation)

## Chunk Architecture Overview

Chunks are fundamental units of data storage within the Swarm network, designed
for efficient handling and robust verification. Each chunk contains a **chunk
body**, which is the core component responsible for storing the actual data and
associated metadata necessary for ensuring data integrity.

### Structure of a Chunk

- **Chunk Body**: At its simplest, a chunk body stores raw data along with
essential metadata, such as a span identifier used for indexing or segmenting
data across distributed networks. The body serves as the primary storage
mechanism, ensuring that data is securely encapsulated and can be efficiently
retrieved. The only body type currently implemented is the Binary Merkle Tree
(BMT), which provides a secure and efficient way to store and retrieve data.

- **Content Chunks**: These chunks directly utilize their bodies to store and
retrieve data. They are designed to handle raw information with minimal
additional features beyond basic data management.

- **Single Owner Chunks (SOCs)**: While building upon the core structure of a
Content Chunk, these chunks enhance their body by incorporating cryptographic
elements for ownership verification. This ensures that only authorized entities
can manage the chunk, adding an extra layer of security and trust.

### Design Benefits

- **Modularity**: The architecture allows different chunk types to encapsulate
varying levels of functionality while maintaining a consistent interface.
Whether it's basic data storage in Content Chunks or enhanced ownership
verification in Single Owner Chunks, the foundational structure remains
adaptable.

- **Scalability**: By focusing on core functionalities within the body and
allowing specific chunk types to extend these basics, the system easily
accommodates new features and requirements without disrupting existing
operations.


### Notes

The BMT body is stored in the database as a tuple in the form of (data, counter).
Counter is used to keep track of the number of times the chunk body has been
referenced by content chunks or single owner chunks. This is used to determine
when the chunk body can be garbage collected.

Chunks (CAC and SOC) are stored with their headers in the database, and the hash
of the chunk body that they link to is stored along with the header. The number
of authorizers for the chunk is also stored along with the header, such that the
tuple (...headers, body_hash, counter) is stored in the database as a value with
the key being the address of the chunk. Alternatively, instead of a counter it
may be a bitfield with the bit positions representing the authorizers of the
chunk.

An authorizer may optionally support authorising a chunk multiple times, in which
case the specific authorizer is responsible for keeping track of the number of
times they have authorised the respective chunk.

An example of an authorizer is a `PostageAuthorizer`. In this case, users purchase
batches of postage stamps which they can use to authorise chunks. A postage stamp
is a signature on the tuple of (chunk_address, batch_id, index, timestamp). The
index is a unique identifier for the stamp within the batch, and the timestamp is
is used to determine if a stamp overwrites a previous stamp. A batch may also be
mutable in which case its depth may be increased and the number of indices
correspondingly increase. The `PostageAuthorizer` should keep a table showing the
number of stamps allocated to each chunk that it has authorised. Batches are
evicted from the database at specific times, at which case the stamps from the
batch are no longer valid. If the number of stamps allocated to a chunk reduces to
zero, the chunk is no longer authorised by the `PostageAuthorizer`. The stamps
retained in a database are stored as a tuple (batch_id, index, timestamp, signature)
and should allow pagination such that one may query for all batches that authorise
a specific chunk.


* Consist of chunk types (e.g. content, soc, etc). May have fields.
* Wrap a chunk body (e.g. BMT body).
* A chunk body may have a property (e.g. span) as well as data of a fixed length.
* Chunk bodies implement their own form of hashing (e.g. BMT).

## Vertex data heirarchy

- Chunks
  - Content Chunks
    - Binary Merkle Tree (BMT) Body
  - Single Owner Chunks
    - Binary Merkle Tree (BMT) Body
