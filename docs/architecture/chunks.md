# Chunk Architecture

Chunks are the fundamental units of data storage within the Swarm network, designed for efficient handling and robust verification.

## Core Concepts

- **Integrity** - Chunk address checking ensures data hasn't been corrupted
- **Authorization** - Stamping provides economic authorization for storage

## Chunk Structure

### Chunk Body

At its simplest, a chunk body stores raw data along with essential metadata, such as a span identifier used for indexing or segmenting data across distributed networks. The body serves as the primary storage mechanism, ensuring that data is securely encapsulated and can be efficiently retrieved.

The only body type currently implemented is the **Binary Merkle Tree (BMT)**, which provides a secure and efficient way to store and retrieve data.

### Content Chunks (CAC)

Content-Addressed Chunks directly utilize their bodies to store and retrieve data. They are designed to handle raw information with minimal additional features beyond basic data management.

The chunk address is derived from the BMT hash of the content, ensuring content integrity.

### Single Owner Chunks (SOC)

While building upon the core structure of a Content Chunk, Single Owner Chunks enhance their body by incorporating cryptographic elements for ownership verification. This ensures that only authorized entities can manage the chunk, adding an extra layer of security and trust.

SOC addresses are derived from both the owner's public key and an identifier, enabling mutable content at stable addresses (used for feeds).

## Data Hierarchy

```
Chunks
├── Content Chunks (CAC)
│   └── Binary Merkle Tree (BMT) Body
└── Single Owner Chunks (SOC)
    └── Binary Merkle Tree (BMT) Body
```

## Design Benefits

### Modularity
The architecture allows different chunk types to encapsulate varying levels of functionality while maintaining a consistent interface. Whether it's basic data storage in Content Chunks or enhanced ownership verification in Single Owner Chunks, the foundational structure remains adaptable.

### Scalability
By focusing on core functionalities within the body and allowing specific chunk types to extend these basics, the system easily accommodates new features and requirements without disrupting existing operations.

## Storage Implementation

### BMT Body Storage

The BMT body is stored in the database as a tuple: `(data, counter)`.

The counter tracks how many times the chunk body has been referenced by content chunks or single owner chunks. This is used to determine when the chunk body can be garbage collected.

### Chunk Header Storage

Chunks (CAC and SOC) are stored with their headers in the database. The tuple stored is:

```
(...headers, body_hash, counter)
```

Where:
- `headers` - Chunk-specific header data
- `body_hash` - Hash of the BMT body this chunk links to
- `counter` - Number of authorizers for the chunk

Alternatively, instead of a counter, a bitfield may be used with bit positions representing the authorizers of the chunk.

## Authorization

An authorizer may optionally support authorizing a chunk multiple times, in which case the specific authorizer is responsible for tracking the number of times they have authorized the respective chunk.

### PostageAuthorizer Example

Users purchase batches of postage stamps to authorize chunks. A postage stamp is a signature on:

```
(chunk_address, batch_id, index, timestamp)
```

Where:
- `index` - Unique identifier for the stamp within the batch
- `timestamp` - Used to determine if a stamp overwrites a previous stamp

A batch may be **mutable**, in which case its depth may be increased and the number of indices correspondingly increase.

The `PostageAuthorizer` maintains a table showing the number of stamps allocated to each chunk it has authorized. Batches are evicted from the database at specific times, at which point stamps from the batch are no longer valid. If the number of stamps allocated to a chunk reduces to zero, the chunk is no longer authorized.

Stamps are stored as:
```
(batch_id, index, timestamp, signature)
```

The storage supports pagination to query all batches that authorize a specific chunk.

## Summary

| Component | Key Properties |
|-----------|---------------|
| **Chunk Body** | Data + span, implements hashing (BMT) |
| **Content Chunk** | Address = BMT hash of content |
| **Single Owner Chunk** | Address = hash(owner_pubkey, identifier), supports mutable content |
| **Authorization** | Postage stamps provide economic authorization |

## See Also

- [Swarm API](../swarm/api.md) - Storage and retrieval traits
- [Differences from Bee](../swarm/differences-from-bee.md) - Postage stamp verification improvements
