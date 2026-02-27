# OpenStack Keystone Integration Reference for Swift API Support

**Document Version:** 1.0
**Date:** 2026-02-27
**Purpose:** Comprehensive reference for implementing OpenStack Swift API with Keystone authentication in RustFS

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Current Keystone Integration (S3 API)](#current-keystone-integration-s3-api)
3. [Swift API Architecture](#swift-api-architecture)
4. [Keystone Integration for Swift](#keystone-integration-for-swift)
5. [Implementation Roadmap](#implementation-roadmap)
6. [Technical Specifications](#technical-specifications)
7. [Ceph RGW Implementation Patterns](#ceph-rgw-implementation-patterns)
8. [Testing Strategy](#testing-strategy)

---

## Executive Summary

### Current State

RustFS has successfully implemented Keystone authentication for the **S3 API** with the following components:

- ✅ **Middleware Layer**: `KeystoneAuthMiddleware` in `rustfs-keystone` crate
- ✅ **Token Validation**: Keystone v3 API integration with token caching
- ✅ **Task-Local Storage**: Async-safe credential passing via `KEYSTONE_CREDENTIALS`
- ✅ **Role-Based Authorization**: Admin/reseller_admin role mapping
- ✅ **Dual Authentication**: Coexists with AWS Signature v4 authentication
- ✅ **Integration Points**: Middleware in HTTP stack, auth handlers in `rustfs/src/auth.rs`

### Target State

Extend Keystone integration to support **OpenStack Swift API** following Ceph RGW patterns:

- 🎯 **Swift Protocol Implementation**: Account/Container/Object hierarchy
- 🎯 **Swift Authentication**: X-Auth-Token header support (already implemented)
- 🎯 **Tenant Isolation**: Project-scoped account URLs (`/v1/AUTH_{project_id}`)
- 🎯 **Unified Namespace**: Swift containers = S3 buckets (bidirectional access)
- 🎯 **Swift-Specific Features**: TempURL, ACLs, Large Objects (SLO/DLO)
- 🎯 **Reseller Prefixes**: Multi-tenant service isolation

---

## Current Keystone Integration (S3 API)

### Architecture Overview

```
HTTP Request with X-Auth-Token
    ↓
KeystoneAuthMiddleware (crates/keystone/src/middleware.rs)
    ├─ Extract X-Auth-Token header
    ├─ Validate token with Keystone v3 API
    ├─ Create Credentials object:
    │  - access_key: "keystone:{user_id}"
    │  - secret_key: "" (empty for token auth)
    │  - claims["keystone_roles"]: ["admin", "member", ...]
    │  - claims["keystone_project_id"]: project UUID
    │  - claims["auth_source"]: "keystone"
    ├─ Store in KEYSTONE_CREDENTIALS task-local storage
    └─ Pass request through to S3 service
    ↓
S3 Service Layer (rustfs/src/server/http.rs)
    ↓
IAMAuth::get_secret_key() (rustfs/src/auth.rs)
    ├─ Check KEYSTONE_CREDENTIALS.try_with() first
    ├─ If credentials present, return empty secret (bypass signature)
    └─ Otherwise, standard IAM auth
    ↓
check_key_valid() (rustfs/src/auth.rs)
    ├─ Check KEYSTONE_CREDENTIALS.try_with()
    ├─ Extract roles from claims["keystone_roles"]
    ├─ Determine is_owner = roles contains "admin" or "reseller_admin"
    └─ Return (Credentials, is_owner)
    ↓
S3 Operation Processing
```

### Key Components

#### 1. Middleware (`crates/keystone/src/middleware.rs`)

**Exports:**
- `KeystoneAuthLayer` - Tower layer for HTTP stack
- `KEYSTONE_CREDENTIALS` - Task-local storage for credentials

**Behavior:**
- Extracts `X-Auth-Token` or `X-Storage-Token` headers
- Validates token with `KeystoneAuthProvider`
- Creates `Credentials` with:
  ```rust
  access_key: format!("keystone:{}", user_id)
  secret_key: String::new()
  session_token: token.clone()
  claims: {
      "keystone_user_id": user_id,
      "keystone_username": username,
      "keystone_project_id": project_id,
      "keystone_project_name": project_name,
      "keystone_roles": ["admin", "member"],
      "auth_source": "keystone"
  }
  ```
- Stores credentials in `KEYSTONE_CREDENTIALS` task-local scope
- Returns 401 with XML error if token invalid

#### 2. Auth Integration (`rustfs/src/auth.rs`)

**Modified Functions:**

**`get_secret_key()`:**
```rust
async fn get_secret_key(&self, access_key: &str) -> S3Result<SecretKey> {
    // Check task-local storage FIRST (handles pure token auth)
    if let Ok(Some(creds)) = KEYSTONE_CREDENTIALS.try_with(|c| c.clone()) {
        return Ok(SecretKey::from(String::new())); // Bypass signature
    }

    // Standard IAM auth continues...
}
```

**`check_key_valid()`:**
```rust
pub async fn check_key_valid(session_token: &str, access_key: &str) -> S3Result<(Credentials, bool)> {
    // Check task-local storage for Keystone credentials
    if let Ok(Some(credentials)) = KEYSTONE_CREDENTIALS.try_with(|creds| creds.clone()) {
        // Extract roles from correct location
        let is_owner = credentials.claims
            .and_then(|claims| claims.get("keystone_roles"))
            .and_then(|roles| roles.as_array())
            .map(|roles| roles.iter().any(|r|
                r.as_str().map(|s| s == "admin" || s == "reseller_admin").unwrap_or(false)
            ))
            .unwrap_or(false);

        return Ok((credentials, is_owner));
    }

    // Standard IAM auth continues...
}
```

#### 3. Configuration (`rustfs/src/main.rs`)

**Initialization:**
```rust
// 3a. Initialize Keystone authentication if enabled
let keystone_config = rustfs_keystone::KeystoneConfig::from_env().map_err(Error::other)?;
if keystone_config.enable {
    match auth_keystone::init_keystone_auth(keystone_config).await {
        Ok(_) => info!("Keystone authentication initialized successfully"),
        Err(e) => {
            error!("Failed to initialize Keystone authentication: {}", e);
            // Continue without Keystone - fall back to standard auth
        }
    }
}
```

**HTTP Server Integration:**
```rust
// In rustfs/src/server/http.rs
let keystone_layer = KeystoneAuthLayer::new(
    crate::auth_keystone::get_keystone_auth()
);

let app = Router::new()
    .route("/", get(s3_service))
    .layer(readiness_gate)
    .layer(keystone_layer)  // Keystone middleware
    .layer(TraceLayer::new_for_http());
```

### Environment Variables

```bash
RUSTFS_KEYSTONE_ENABLE=true
RUSTFS_KEYSTONE_AUTH_URL=http://keystone:5000
RUSTFS_KEYSTONE_VERSION=v3
RUSTFS_KEYSTONE_ADMIN_USER=admin
RUSTFS_KEYSTONE_ADMIN_PASSWORD=secret
RUSTFS_KEYSTONE_ADMIN_PROJECT=admin
RUSTFS_KEYSTONE_ADMIN_DOMAIN=Default
RUSTFS_KEYSTONE_CACHE_SIZE=10000
RUSTFS_KEYSTONE_CACHE_TTL=300
RUSTFS_KEYSTONE_VERIFY_SSL=true
```

### Credentials Structure

```rust
Credentials {
    access_key: "keystone:550e8400-e29b-41d4-a716-446655440000",
    secret_key: "",  // Empty - token auth doesn't use secrets
    session_token: "gAAAAABe1x2y3z4...",  // Keystone token
    expiration: Some(OffsetDateTime::parse("2026-02-28T10:30:00Z")),
    status: "active",
    parent_user: "admin",
    groups: Some(vec!["admin", "member"]),  // Keystone roles
    claims: Some({
        "keystone_user_id": "550e8400-e29b-41d4-a716-446655440000",
        "keystone_username": "admin",
        "keystone_project_id": "7188e165c0ae4424ac68ae2e89a05c50",
        "keystone_project_name": "demo",
        "keystone_domain_id": "default",
        "keystone_domain_name": "Default",
        "keystone_roles": ["admin", "member"],
        "auth_source": "keystone"
    }),
    name: None,
    description: None,
}
```

---

## Swift API Architecture

### Hierarchy Model

**Swift has a three-level hierarchy (vs S3's bucket-centric model):**

```
Account: /v1/AUTH_{project_uuid}
├── Container: photos
│   ├── Object: vacation/beach.jpg
│   ├── Object: vacation/sunset.png
│   └── Object: profile.jpg
├── Container: documents
│   ├── Object: report.pdf
│   └── Object: invoice.pdf
└── Container: backups
    └── Object: 2026-02-27/data.tar.gz
```

**Key Concepts:**

| Swift Concept | Equivalent S3 | Description |
|---------------|---------------|-------------|
| Account | AWS Account | Top-level namespace, maps to Keystone project |
| Container | Bucket | Holds objects, unique within account |
| Object | Object | Actual data, unique within container |
| `AUTH_` prefix | N/A | "Reseller prefix" for tenant isolation |

### URL Patterns

**Swift Account Operations:**
```
GET    /v1/{account}                    # List containers
HEAD   /v1/{account}                    # Account metadata
POST   /v1/{account}                    # Update account metadata
DELETE /v1/{account}                    # Delete account (admin)
```

**Swift Container Operations:**
```
GET    /v1/{account}/{container}        # List objects
PUT    /v1/{account}/{container}        # Create container
POST   /v1/{account}/{container}        # Update container metadata
HEAD   /v1/{account}/{container}        # Container metadata
DELETE /v1/{account}/{container}        # Delete container (must be empty)
```

**Swift Object Operations:**
```
GET    /v1/{account}/{container}/{object}  # Download object
PUT    /v1/{account}/{container}/{object}  # Upload object
POST   /v1/{account}/{container}/{object}  # Update object metadata
HEAD   /v1/{account}/{container}/{object}  # Object metadata
DELETE /v1/{account}/{container}/{object}  # Delete object
COPY   /v1/{account}/{container}/{object}  # Copy object (server-side)
```

### Swift-Specific Headers

**Authentication:**
- `X-Auth-Token` - Keystone token (primary)
- `X-Storage-Token` - Alternative token header (legacy)
- `X-Service-Token` - Service token for expired auth extension

**Response Headers:**
- `X-Storage-Url` - Account endpoint URL after authentication
- `X-Trans-Id` - Unique transaction identifier
- `X-Timestamp` - UNIX Epoch timestamp
- `X-Openstack-Request-Id` - OpenStack compatibility

**Metadata Headers:**
- `X-Container-Meta-{key}` - Container custom metadata
- `X-Object-Meta-{key}` - Object custom metadata
- `X-Account-Meta-{key}` - Account custom metadata

**ACL Headers:**
- `X-Container-Read` - Read access control list
- `X-Container-Write` - Write access control list

**Special Features:**
- `X-Object-Manifest` - Dynamic Large Object (DLO) manifest
- `X-Copy-From` - Source for server-side copy
- `X-Versions-Location` - Object versioning container
- `X-Account-Meta-Temp-URL-Key` - TempURL secret key
- `X-Delete-After` - Auto-delete object after N seconds

### Swift vs S3 Protocol Differences

| Aspect | Swift | S3 | Implication for RustFS |
|--------|-------|----|-----------------------|
| **Authentication** | Token-based (X-Auth-Token) | Signature-based (AWS SigV4) | ✅ Already handled by Keystone middleware |
| **Hierarchy** | Account/Container/Object | Bucket/Object (flat) | 🎯 Need routing layer for accounts |
| **Metadata** | X-Container-Meta-*, X-Object-Meta-* | x-amz-meta-* | 🎯 Header translation needed |
| **ACLs** | X-Container-Read/Write | XML ACL documents | 🎯 New ACL implementation |
| **Large Objects** | SLO/DLO with manifest | Multipart uploads | 🎯 New upload mechanism |
| **Listing** | format=json/xml/plain | Always XML | 🎯 Response format negotiation |
| **Namespacing** | Account prefix (AUTH_) | Bucket globally unique | ✅ Tenant isolation via project_id |

---

## Keystone Integration for Swift

### Authentication Flow

**Current S3 API Flow (Already Implemented):**
```
1. Client → RustFS: GET /bucket/object with X-Auth-Token
2. KeystoneAuthMiddleware → Keystone: Validate token
3. Keystone → Middleware: Token valid, return user/project/roles
4. Middleware: Create Credentials, store in KEYSTONE_CREDENTIALS
5. Middleware → S3 Service: Pass request through
6. S3 Service: Process with Keystone credentials
```

**Target Swift API Flow:**
```
1. Client → RustFS: GET /v1/AUTH_{project_id}/container/object with X-Auth-Token
2. KeystoneAuthMiddleware → Keystone: Validate token (SAME AS S3)
3. Keystone → Middleware: Token valid, return user/project/roles (SAME AS S3)
4. Middleware: Create Credentials, store in KEYSTONE_CREDENTIALS (SAME AS S3)
5. Middleware → Swift Service: Pass request through (NEW ROUTING)
6. Swift Service: Extract account from URL, verify matches project_id
7. Swift Service: Map container to S3 bucket (TRANSLATION LAYER)
8. Swift Service: Process operation with Keystone credentials (REUSE S3 BACKEND)
```

### Key Insight: Reuse Existing Keystone Integration

**The middleware layer is ALREADY COMPLETE and works for Swift!**

The `KeystoneAuthMiddleware` is protocol-agnostic:
- ✅ Handles `X-Auth-Token` header (Swift uses this)
- ✅ Validates with Keystone v3 API (Swift uses this)
- ✅ Stores credentials in task-local storage (Swift can access this)
- ✅ Returns 401 for invalid tokens (Swift needs this)

**What's needed for Swift:**
1. 🎯 **Swift Router**: New routing layer for `/v1/{account}` paths
2. 🎯 **Account Validation**: Verify URL account matches token's project_id
3. 🎯 **Container-Bucket Mapping**: Translate Swift containers ↔ S3 buckets
4. 🎯 **Header Translation**: Map Swift headers to S3 equivalents
5. 🎯 **Response Formatting**: Support JSON/XML/plain text responses

### Tenant Isolation Mechanism

**Ceph RGW Pattern:**
```
External URL:  /v1/AUTH_7188e165c0ae4424ac68ae2e89a05c50/photos/beach.jpg
               ↓
Internal RGW:  tenant_id=7188e165c0ae4424ac68ae2e89a05c50
               container="photos"
               object="beach.jpg"
               ↓
RADOS Storage: bucket="7188e165c0ae4424ac68ae2e89a05c50/photos"
               object="beach.jpg"
```

**RustFS Implementation Pattern:**
```
Swift URL:    /v1/AUTH_7188e165c0ae4424ac68ae2e89a05c50/photos/beach.jpg
              ↓
Middleware:   Extract token, validate with Keystone
              Store project_id in KEYSTONE_CREDENTIALS
              ↓
Swift Router: Parse URL → account="AUTH_7188e165c0ae4424ac68ae2e89a05c50"
              Verify: account_project_id == credentials.project_id
              ↓
Translation:  Swift container="photos"
              ↓
              S3 bucket name with tenant prefix:
              bucket = "{project_id}:photos"
              OR bucket = "photos" (if tenant prefixing disabled)
              ↓
              Object key = "beach.jpg"
              ↓
S3 Backend:   Call existing ECStore with:
              bucket="7188e165c0ae4424ac68ae2e89a05c50:photos"
              key="beach.jpg"
              credentials=KEYSTONE_CREDENTIALS
```

### Unified Namespace Strategy

**Goal:** Swift containers and S3 buckets are the SAME underlying objects.

**Option 1: Tenant-Prefixed Buckets (Ceph Pattern)**
```
Swift: /v1/AUTH_project123/photos → Backend: "project123:photos"
S3:    /photos → Backend: "photos"
Problem: Different namespaces, not truly unified
```

**Option 2: Account-Scoped Buckets (Recommended)**
```
Swift: /v1/AUTH_project123/photos → Backend: "photos" (scoped to project123)
S3:    /photos → Backend: "photos" (scoped to IAM user's context)
Solution: Use credentials.project_id for isolation at query level
```

**Implementation:**
```rust
// In Swift handler
async fn list_containers(credentials: &Credentials) -> Result<Vec<Container>> {
    let project_id = credentials.claims
        .as_ref()
        .and_then(|c| c.get("keystone_project_id"))
        .and_then(|v| v.as_str())
        .ok_or(Error::InvalidToken)?;

    // Query buckets filtered by project metadata
    let buckets = ecstore.list_buckets_for_tenant(project_id).await?;

    // Or use naming convention
    let prefix = format!("{}:", project_id);
    let buckets = ecstore.list_buckets_with_prefix(&prefix).await?;

    Ok(buckets.into_iter().map(|b| Container::from_bucket(b)).collect())
}
```

### Configuration Additions

**New Environment Variables for Swift:**
```bash
# Swift API control
RUSTFS_SWIFT_ENABLE=true
RUSTFS_SWIFT_URL_PREFIX=swift  # Optional: /swift/v1/... instead of /v1/...

# Tenant isolation strategy
RUSTFS_KEYSTONE_IMPLICIT_TENANTS=true  # Auto-create tenant-based users
RUSTFS_KEYSTONE_TENANT_PREFIX=true     # Use project_id:bucket naming

# Swift-specific features
RUSTFS_SWIFT_ACCOUNT_IN_URL=true       # Enable /v1/{account} URLs
RUSTFS_SWIFT_TOKEN_EXPIRATION=86400    # Token validity in seconds
RUSTFS_SWIFT_VERSIONING_ENABLED=true   # Enable object versioning
```

**Add to `KeystoneConfig` struct:**
```rust
pub struct KeystoneConfig {
    // Existing S3 fields...
    pub enable: bool,
    pub auth_url: String,
    pub version: String,
    // ... existing fields ...

    // NEW: Swift-specific fields
    pub enable_swift: bool,                    // Enable Swift API
    pub swift_url_prefix: Option<String>,       // URL prefix for Swift
    pub implicit_tenants: bool,                 // Auto-create tenant users
    pub tenant_prefix: bool,                    // Use tenant prefixing
    pub swift_account_in_url: bool,             // Support /v1/{account} URLs
    pub swift_token_expiration: u32,            // Token validity seconds
    pub swift_versioning_enabled: bool,         // Enable versioning
}
```

---

## Implementation Roadmap

### Phase 1: Swift Router Infrastructure (Week 1)

**Objective:** Basic Swift URL routing and account handling

**Tasks:**
1. Create `rustfs/src/swift/` module directory
   - `mod.rs` - Module exports
   - `router.rs` - Swift URL routing
   - `account.rs` - Account operations
   - `container.rs` - Container operations (placeholder)
   - `object.rs` - Object operations (placeholder)
   - `errors.rs` - Swift error responses

2. Implement Swift Router
   ```rust
   // rustfs/src/swift/router.rs
   pub struct SwiftRouter {
       enable: bool,
       url_prefix: Option<String>,
       account_pattern: Regex,  // /v1/AUTH_{uuid}
   }

   impl SwiftRouter {
       pub fn route(&self, uri: &Uri) -> Option<SwiftRoute> {
           // Parse: /v1/AUTH_{uuid}/container/object
           // Return: SwiftRoute { account, container?, object? }
       }
   }
   ```

3. Integrate into HTTP server
   ```rust
   // rustfs/src/server/http.rs
   async fn handle_request(req: Request) -> Response {
       // Check if Swift URL pattern
       if let Some(swift_route) = swift_router.route(req.uri()) {
           return handle_swift_request(swift_route, req).await;
       }

       // Otherwise, handle as S3 request
       handle_s3_request(req).await
   }
   ```

4. Account validation
   ```rust
   // rustfs/src/swift/account.rs
   pub fn validate_account_access(
       account: &str,
       credentials: &Credentials
   ) -> Result<String> {
       // Extract project_id from account URL
       let account_project_id = account.strip_prefix("AUTH_")
           .ok_or(Error::InvalidAccount)?;

       // Get project_id from credentials
       let cred_project_id = credentials.claims
           .as_ref()
           .and_then(|c| c.get("keystone_project_id"))
           .and_then(|v| v.as_str())
           .ok_or(Error::Unauthorized)?;

       // Verify match
       if account_project_id != cred_project_id {
           return Err(Error::Forbidden);
       }

       Ok(account_project_id.to_string())
   }
   ```

**Deliverables:**
- ✅ Swift URL routing working
- ✅ Account validation integrated with Keystone credentials
- ✅ Basic 404 responses for unimplemented operations
- ✅ Integration tests for routing logic

### Phase 2: Container Operations (Week 2)

**Objective:** Implement Swift container CRUD with S3 backend mapping

**Tasks:**
1. Container-Bucket Translation Layer
   ```rust
   // rustfs/src/swift/container.rs
   pub struct ContainerMapper {
       tenant_prefix_enabled: bool,
   }

   impl ContainerMapper {
       pub fn swift_to_s3_bucket(
           &self,
           container: &str,
           project_id: &str
       ) -> String {
           if self.tenant_prefix_enabled {
               format!("{}:{}", project_id, container)
           } else {
               container.to_string()
           }
       }

       pub fn s3_to_swift_container(
           &self,
           bucket: &str,
           project_id: &str
       ) -> String {
           if self.tenant_prefix_enabled {
               bucket.strip_prefix(&format!("{}:", project_id))
                   .unwrap_or(bucket)
                   .to_string()
           } else {
               bucket.to_string()
           }
       }
   }
   ```

2. List Containers (GET /v1/{account})
   ```rust
   pub async fn list_containers(
       account: &str,
       credentials: &Credentials,
       store: &ECStore,
   ) -> Result<Vec<Container>> {
       let project_id = validate_account_access(account, credentials)?;

       // Query S3 buckets filtered by tenant
       let buckets = if tenant_prefix_enabled {
           let prefix = format!("{}:", project_id);
           store.list_buckets_with_prefix(&prefix).await?
       } else {
           // Use metadata filtering
           store.list_buckets_for_tenant(&project_id).await?
       };

       // Convert to Swift containers
       Ok(buckets.into_iter()
           .map(|b| Container::from_bucket(b, &project_id))
           .collect())
   }
   ```

3. Create Container (PUT /v1/{account}/{container})
   ```rust
   pub async fn create_container(
       account: &str,
       container: &str,
       headers: &HeaderMap,
       credentials: &Credentials,
       store: &ECStore,
   ) -> Result<()> {
       let project_id = validate_account_access(account, credentials)?;
       let bucket_name = mapper.swift_to_s3_bucket(container, &project_id);

       // Call existing S3 bucket creation
       let bucket_options = BucketOptions {
           // Extract metadata from X-Container-Meta-* headers
           metadata: extract_container_metadata(headers),
           // Extract ACLs from X-Container-Read/Write headers
           acl: extract_container_acl(headers),
           ..Default::default()
       };

       store.create_bucket(&bucket_name, bucket_options).await?;
       Ok(())
   }
   ```

4. Delete Container (DELETE /v1/{account}/{container})
5. Container Metadata (HEAD /v1/{account}/{container})
6. Update Container Metadata (POST /v1/{account}/{container})

**Deliverables:**
- ✅ Container CRUD operations working
- ✅ S3 bucket backend integration
- ✅ Tenant isolation enforced
- ✅ Swift metadata headers handled
- ✅ Integration tests with real ECStore

### Phase 3: Object Operations (Week 3)

**Objective:** Implement Swift object operations with S3 backend

**Tasks:**
1. Object-Key Translation
   ```rust
   // Swift object names are the S3 keys as-is
   // No translation needed except URL decoding
   pub fn swift_object_to_s3_key(object: &str) -> String {
       urlencoding::decode(object).unwrap_or(object.to_string())
   }
   ```

2. Upload Object (PUT /v1/{account}/{container}/{object})
   ```rust
   pub async fn put_object(
       account: &str,
       container: &str,
       object: &str,
       body: Bytes,
       headers: &HeaderMap,
       credentials: &Credentials,
       store: &ECStore,
   ) -> Result<()> {
       let project_id = validate_account_access(account, credentials)?;
       let bucket = mapper.swift_to_s3_bucket(container, &project_id);
       let key = swift_object_to_s3_key(object);

       // Extract Swift metadata
       let metadata = extract_object_metadata(headers);

       // Call existing S3 PutObject
       store.put_object(
           &bucket,
           &key,
           body,
           ObjectOptions {
               metadata,
               content_type: headers.get("content-type"),
               ..Default::default()
           }
       ).await?;

       Ok(())
   }
   ```

3. Download Object (GET /v1/{account}/{container}/{object})
4. Delete Object (DELETE /v1/{account}/{container}/{object})
5. Object Metadata (HEAD /v1/{account}/{container}/{object})
6. Update Object Metadata (POST /v1/{account}/{container}/{object})
7. Copy Object (COPY /v1/{account}/{container}/{object})
   - Handle X-Copy-From header
   - Server-side copy within RustFS

**Deliverables:**
- ✅ Object CRUD operations working
- ✅ Metadata handling (X-Object-Meta-*)
- ✅ Server-side copy implemented
- ✅ Range requests supported
- ✅ Integration tests with object operations

### Phase 4: Response Formatting (Week 4)

**Objective:** Swift-specific response formats (JSON/XML/plain)

**Tasks:**
1. Format Negotiation
   ```rust
   pub enum ResponseFormat {
       Json,
       Xml,
       PlainText,
   }

   pub fn negotiate_format(
       query: Option<&str>,
       headers: &HeaderMap
   ) -> ResponseFormat {
       // Check ?format=json|xml|plain query parameter
       if let Some(q) = query {
           if q.contains("format=json") { return ResponseFormat::Json; }
           if q.contains("format=xml") { return ResponseFormat::Xml; }
       }

       // Check Accept header
       if let Some(accept) = headers.get("accept") {
           if accept.to_str().unwrap_or("").contains("application/json") {
               return ResponseFormat::Json;
           }
           if accept.to_str().unwrap_or("").contains("application/xml") {
               return ResponseFormat::Xml;
           }
       }

       // Default to plain text
       ResponseFormat::PlainText
   }
   ```

2. Container Listing Formats
   ```rust
   // Plain text: one container per line
   pub fn format_containers_plain(containers: &[Container]) -> String {
       containers.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join("\n")
   }

   // JSON: array of objects with metadata
   pub fn format_containers_json(containers: &[Container]) -> Value {
       json!(containers.iter().map(|c| json!({
           "name": c.name,
           "count": c.object_count,
           "bytes": c.bytes_used,
           "last_modified": c.last_modified.format(&Rfc3339),
       })).collect::<Vec<_>>())
   }

   // XML: OpenStack-style XML response
   pub fn format_containers_xml(containers: &[Container]) -> String {
       let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<account>");
       for c in containers {
           xml.push_str(&format!(
               "<container><name>{}</name><count>{}</count><bytes>{}</bytes></container>",
               escape_xml(&c.name), c.object_count, c.bytes_used
           ));
       }
       xml.push_str("</account>");
       xml
   }
   ```

3. Object Listing Formats (similar pattern)
4. Error Response Formats
   ```rust
   pub fn format_error_response(
       error: &SwiftError,
       format: ResponseFormat
   ) -> Response {
       match format {
           ResponseFormat::Json => json_error_response(error),
           ResponseFormat::Xml => xml_error_response(error),
           ResponseFormat::PlainText => plain_error_response(error),
       }
   }
   ```

**Deliverables:**
- ✅ All three response formats working
- ✅ Format negotiation from query/headers
- ✅ OpenStack-compatible responses
- ✅ Tests for each format

### Phase 5: Swift-Specific Features (Week 5-6)

**Objective:** Implement Swift differentiating features

**Tasks:**
1. **TempURL Support**
   - Account metadata for temp URL keys (X-Account-Meta-Temp-URL-Key)
   - Signature validation (HMAC-SHA1/256/512)
   - Query parameter extraction (temp_url_sig, temp_url_expires)

2. **Static Large Objects (SLO)**
   - Manifest JSON parsing
   - Multi-segment downloads
   - Segment validation (ETags, sizes)

3. **Dynamic Large Objects (DLO)**
   - X-Object-Manifest header handling
   - Container prefix-based segment discovery
   - Concatenated streaming responses

4. **Swift ACLs**
   - X-Container-Read/Write header parsing
   - Format: `.r:*,.rlistings,project:user`
   - ACL enforcement in authorization layer

5. **Object Versioning**
   - X-Versions-Location header
   - Archive container for old versions
   - Version restoration

**Deliverables:**
- ✅ TempURL working with key rotation
- ✅ SLO/DLO upload and download
- ✅ Swift ACLs enforced
- ✅ Object versioning functional

### Phase 6: Testing & Documentation (Week 7)

**Objective:** Comprehensive testing and documentation

**Tasks:**
1. **Unit Tests**
   - URL parsing and routing
   - Container-bucket translation
   - Format negotiation
   - Header extraction
   - ACL parsing

2. **Integration Tests**
   - Full Swift API workflows
   - Keystone authentication flows
   - S3-Swift interoperability
   - Tenant isolation verification

3. **End-to-End Tests**
   - python-swiftclient compatibility
   - OpenStack SDK compatibility
   - Performance benchmarks

4. **Documentation**
   - Swift API reference
   - Configuration guide
   - Migration guide (Ceph RGW → RustFS)
   - Troubleshooting guide

**Deliverables:**
- ✅ 80%+ code coverage
- ✅ E2E tests passing
- ✅ Complete documentation
- ✅ Performance benchmarks

---

## Technical Specifications

### URL Routing Patterns

**Swift URL Structure:**
```
/v1/{account}/{container?}/{object?}{query}
```

**Examples:**
```
/v1/AUTH_7188e165c0ae4424ac68ae2e89a05c50
/v1/AUTH_7188e165c0ae4424ac68ae2e89a05c50?format=json
/v1/AUTH_7188e165c0ae4424ac68ae2e89a05c50/photos
/v1/AUTH_7188e165c0ae4424ac68ae2e89a05c50/photos?prefix=vacation&limit=100
/v1/AUTH_7188e165c0ae4424ac68ae2e89a05c50/photos/beach.jpg
/v1/AUTH_7188e165c0ae4424ac68ae2e89a05c50/photos/vacation/sunset.png?temp_url_sig=...
```

**Routing Decision Tree:**
```rust
match (method, path_segments) {
    // Account operations
    (Method::GET,  ["v1", account])                    => list_containers(account),
    (Method::HEAD, ["v1", account])                    => account_metadata(account),
    (Method::POST, ["v1", account])                    => update_account_metadata(account),
    (Method::DELETE, ["v1", account])                  => delete_account(account),

    // Container operations
    (Method::GET,  ["v1", account, container])         => list_objects(account, container),
    (Method::PUT,  ["v1", account, container])         => create_container(account, container),
    (Method::HEAD, ["v1", account, container])         => container_metadata(account, container),
    (Method::POST, ["v1", account, container])         => update_container_metadata(account, container),
    (Method::DELETE, ["v1", account, container])       => delete_container(account, container),

    // Object operations
    (Method::GET,  ["v1", account, container, object @ ..])  => get_object(account, container, object),
    (Method::PUT,  ["v1", account, container, object @ ..])  => put_object(account, container, object),
    (Method::HEAD, ["v1", account, container, object @ ..])  => object_metadata(account, container, object),
    (Method::POST, ["v1", account, container, object @ ..])  => update_object_metadata(account, container, object),
    (Method::DELETE, ["v1", account, container, object @ ..]) => delete_object(account, container, object),
    (Method::COPY, ["v1", account, container, object @ ..])  => copy_object(account, container, object),

    _ => Err(Error::NotFound),
}
```

### Header Translation Mappings

| Swift Header | S3 Equivalent | Translation |
|--------------|---------------|-------------|
| X-Container-Meta-{key} | x-amz-meta-{key} | Prefix replacement |
| X-Object-Meta-{key} | x-amz-meta-{key} | Prefix replacement |
| X-Container-Read | Bucket Policy (read) | ACL translation |
| X-Container-Write | Bucket Policy (write) | ACL translation |
| X-Delete-After | x-amz-expiration | Timestamp conversion |
| X-Delete-At | x-amz-expiration | Direct mapping |
| Content-Type | Content-Type | Pass through |
| Content-Length | Content-Length | Pass through |
| ETag | ETag | Pass through |
| X-Timestamp | x-amz-meta-x-timestamp | Store as metadata |

### Metadata Storage Strategy

**Option 1: S3 Metadata (Recommended)**
```rust
// Store Swift metadata as S3 metadata with prefix
s3_metadata: {
    "x-amz-meta-x-container-meta-color": "blue",
    "x-amz-meta-x-object-meta-author": "john",
}

// Pro: Works with existing S3 API
// Con: Metadata keys prefixed twice
```

**Option 2: Custom Metadata Table**
```rust
// Store in separate metadata store
swift_metadata: {
    container: "photos",
    metadata: {
        "color": "blue",
        "created-by": "john"
    }
}

// Pro: Clean storage, Swift-native
// Con: Requires new storage backend
```

**Recommendation:** Use Option 1 initially, migrate to Option 2 if needed for performance.

### Response Headers

**Standard Swift Response Headers:**
```rust
let mut response = Response::new(body);
response.headers_mut().insert("content-type", ContentType::from_format(format));
response.headers_mut().insert("x-trans-id", generate_transaction_id());
response.headers_mut().insert("x-openstack-request-id", trans_id.clone());
response.headers_mut().insert("date", httpdate::fmt_http_date(SystemTime::now()));

// For containers/objects
response.headers_mut().insert("x-timestamp", object.created_at.unix_timestamp());
response.headers_mut().insert("etag", object.etag);
response.headers_mut().insert("content-length", object.size);
```

### Error Response Format

**Swift Error Response (Plain Text):**
```
401 Unauthorized
```

**Swift Error Response (JSON):**
```json
{
  "error": {
    "code": 401,
    "title": "Unauthorized",
    "message": "Invalid authentication token"
  }
}
```

**Swift Error Response (XML):**
```xml
<?xml version="1.0" encoding="UTF-8"?>
<error>
  <code>401</code>
  <title>Unauthorized</title>
  <message>Invalid authentication token</message>
</error>
```

---

## Ceph RGW Implementation Patterns

### Configuration Reference

**Key Ceph RGW Configuration Options:**
```ini
[client.radosgw.gateway]
# Swift API settings
rgw_swift_url = http://swift.example.com
rgw_swift_url_prefix = swift          # Optional: /swift/v1 instead of /v1
rgw_swift_auth_url = http://swift.example.com/auth
rgw_swift_auth_entry = auth
rgw_swift_account_in_url = true       # Enable /v1/{account} URLs
rgw_swift_token_expiration = 86400    # Token validity: 24 hours
rgw_swift_versioning_enabled = true
rgw_enforce_swift_acls = true

# Keystone integration
rgw_keystone_url = http://keystone:35357
rgw_keystone_api_version = 3
rgw_keystone_accepted_roles = Member, admin
rgw_keystone_token_cache_size = 10000
rgw_keystone_implicit_tenants = true  # Auto-create tenant users
rgw_keystone_verify_ssl = true
rgw_keystone_service_token_enabled = false
rgw_s3_auth_use_keystone = false      # S3 uses IAM, not Keystone

# Tenant/multi-tenancy settings
rgw_keystone_revocation_interval = 900
rgw_keystone_token_cache_expiration = 900
```

### User Management Pattern

**Ceph RGW Auto-User Creation:**
When Keystone authentication succeeds, RGW automatically creates a user:
- User ID format: `{tenant_id}${tenant_id}`
- Example: `7188e165c0ae4424ac68ae2e89a05c50$7188e165c0ae4424ac68ae2e89a05c50`
- No manual user creation needed
- Users are ephemeral (exist only during token validity)

**RustFS Pattern:**
```rust
pub async fn ensure_keystone_user(
    credentials: &Credentials,
    iam_store: &IAMStore
) -> Result<()> {
    let project_id = credentials.claims
        .as_ref()
        .and_then(|c| c.get("keystone_project_id"))
        .and_then(|v| v.as_str())
        .ok_or(Error::MissingProjectId)?;

    // Create user ID in Ceph format
    let user_id = format!("{}${}", project_id, project_id);

    // Check if user exists
    if iam_store.get_user(&user_id).await.is_err() {
        // Create ephemeral Keystone user
        iam_store.create_keystone_user(User {
            id: user_id,
            display_name: credentials.parent_user.clone(),
            access_key: credentials.access_key.clone(),
            tenant_id: Some(project_id.to_string()),
            is_keystone: true,
            ..Default::default()
        }).await?;
    }

    Ok(())
}
```

### Container/Bucket Naming

**Ceph RGW Naming Convention:**
```
Swift Container: "photos"
Keystone Project: "7188e165c0ae4424ac68ae2e89a05c50"

Internal RADOS:
- Pool: .rgw.buckets
- Bucket marker: 7188e165c0ae4424ac68ae2e89a05c50/photos
- Bucket instance: 7188e165c0ae4424ac68ae2e89a05c50/photos.123456
```

**RustFS Naming Pattern:**
```rust
// Option 1: Prefix-based (simple)
fn bucket_name_with_tenant(container: &str, project_id: &str) -> String {
    format!("{}:{}", project_id, container)
}

// Option 2: Metadata-based (flexible)
fn bucket_name_isolated(container: &str) -> String {
    container.to_string()  // Store project_id in bucket metadata
}

// Query buckets for tenant
async fn list_tenant_buckets(
    project_id: &str,
    store: &ECStore
) -> Result<Vec<Bucket>> {
    store.list_buckets()
        .await?
        .into_iter()
        .filter(|b| b.metadata.get("keystone_project_id") == Some(project_id))
        .collect()
}
```

### Token Caching Strategy

**Ceph RGW Token Cache:**
- Size: 10,000 tokens (default)
- Expiration: Token's own expiration timestamp
- Eviction: LRU (Least Recently Used)
- Cache key: Token string
- Cache value: User info + roles + project

**RustFS Implementation (Already Exists):**
```rust
// crates/keystone/src/lib.rs
pub struct TokenCache {
    cache: Cache<String, Arc<KeystoneToken>>,
}

impl TokenCache {
    pub fn new(capacity: u64, ttl: Duration) -> Self {
        Self {
            cache: Cache::builder()
                .max_capacity(capacity)
                .time_to_live(ttl)
                .build(),
        }
    }

    pub async fn get(&self, token: &str) -> Option<Arc<KeystoneToken>> {
        let cached = self.cache.get(token).await?;
        if cached.is_expired() {
            self.cache.invalidate(token).await;
            return None;
        }
        Some(cached)
    }
}
```

**Already implemented! ✅ No changes needed for Swift.**

### ACL Translation

**Ceph RGW Swift ACL Format:**
```
X-Container-Read: .r:*,.rlistings,7188e165:user123,project456:*
```

**Mapping to S3 Policy:**
```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Principal": "*",
      "Action": ["s3:GetObject"],
      "Resource": "arn:aws:s3:::bucket/*"
    },
    {
      "Effect": "Allow",
      "Principal": "*",
      "Action": ["s3:ListBucket"],
      "Resource": "arn:aws:s3:::bucket"
    },
    {
      "Effect": "Allow",
      "Principal": {
        "AWS": "arn:aws:iam::7188e165:user/user123"
      },
      "Action": ["s3:GetObject", "s3:ListBucket"],
      "Resource": ["arn:aws:s3:::bucket", "arn:aws:s3:::bucket/*"]
    },
    {
      "Effect": "Allow",
      "Principal": {
        "AWS": "arn:aws:iam::project456:user/*"
      },
      "Action": ["s3:GetObject", "s3:ListBucket"],
      "Resource": ["arn:aws:s3:::bucket", "arn:aws:s3:::bucket/*"]
    }
  ]
}
```

**Implementation:**
```rust
pub fn parse_swift_acl(acl_header: &str) -> Vec<AclGrant> {
    let mut grants = Vec::new();

    for element in acl_header.split(',') {
        let element = element.trim();

        if element == ".r:*" {
            // Public read access
            grants.push(AclGrant {
                grantee: Grantee::AllUsers,
                permission: Permission::Read,
            });
        } else if element == ".rlistings" {
            // Public listing
            grants.push(AclGrant {
                grantee: Grantee::AllUsers,
                permission: Permission::ListBucket,
            });
        } else if element.contains(':') {
            // project:user format
            let parts: Vec<&str> = element.split(':').collect();
            if parts.len() == 2 {
                let (project, user) = (parts[0], parts[1]);
                if user == "*" {
                    // All users in project
                    grants.push(AclGrant {
                        grantee: Grantee::Project(project.to_string()),
                        permission: Permission::Read,
                    });
                } else {
                    // Specific user in project
                    grants.push(AclGrant {
                        grantee: Grantee::User {
                            project: project.to_string(),
                            user: user.to_string(),
                        },
                        permission: Permission::Read,
                    });
                }
            }
        }
    }

    grants
}
```

---

## Testing Strategy

### Unit Tests

**Test Coverage Areas:**
1. URL parsing and routing
2. Account name validation
3. Container-bucket translation
4. Header extraction (metadata, ACLs)
5. Response format negotiation
6. ACL parsing and translation
7. TempURL signature validation
8. SLO/DLO manifest parsing

**Example Test:**
```rust
#[test]
fn test_swift_url_parsing() {
    let router = SwiftRouter::new();

    // Test account URL
    let route = router.parse("/v1/AUTH_project123").unwrap();
    assert_eq!(route.account, "AUTH_project123");
    assert!(route.container.is_none());
    assert!(route.object.is_none());

    // Test container URL
    let route = router.parse("/v1/AUTH_project123/photos").unwrap();
    assert_eq!(route.account, "AUTH_project123");
    assert_eq!(route.container, Some("photos"));
    assert!(route.object.is_none());

    // Test object URL
    let route = router.parse("/v1/AUTH_project123/photos/beach.jpg").unwrap();
    assert_eq!(route.account, "AUTH_project123");
    assert_eq!(route.container, Some("photos"));
    assert_eq!(route.object, Some("beach.jpg"));
}
```

### Integration Tests

**Test Scenarios:**
1. **Authentication Flow**
   - Valid Keystone token → Success
   - Invalid token → 401 Unauthorized
   - Expired token → 401 Unauthorized
   - Token for different project → 403 Forbidden

2. **Container Operations**
   - Create container → Verify S3 bucket created
   - List containers → Verify only tenant's containers shown
   - Delete container → Verify S3 bucket deleted
   - Container metadata → Verify round-trip

3. **Object Operations**
   - Upload object → Verify S3 object created
   - Download object → Verify content matches
   - Object metadata → Verify round-trip
   - Delete object → Verify S3 object deleted

4. **S3-Swift Interoperability**
   - Create bucket via S3 → List via Swift
   - Upload object via Swift → Download via S3
   - Delete via S3 → Verify gone in Swift

5. **Tenant Isolation**
   - Project A cannot list Project B's containers
   - Project A cannot access Project B's objects
   - Admin role can access all projects (if configured)

**Example Integration Test:**
```rust
#[tokio::test]
async fn test_swift_container_operations() {
    // Setup
    let keystone_token = authenticate_with_keystone("admin", "secret").await;
    let project_id = extract_project_id(&keystone_token);

    // Create container
    let response = client
        .put(&format!("/v1/AUTH_{}/test-container", project_id))
        .header("X-Auth-Token", &keystone_token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    // List containers
    let response = client
        .get(&format!("/v1/AUTH_{}", project_id))
        .header("X-Auth-Token", &keystone_token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let containers: Vec<String> = response.text().await.unwrap()
        .lines()
        .map(String::from)
        .collect();
    assert!(containers.contains(&"test-container".to_string()));

    // Delete container
    let response = client
        .delete(&format!("/v1/AUTH_{}/test-container", project_id))
        .header("X-Auth-Token", &keystone_token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}
```

### End-to-End Tests

**Test with Real Clients:**
1. **python-swiftclient**
   ```bash
   export OS_AUTH_URL=http://keystone:5000/v3
   export OS_USERNAME=admin
   export OS_PASSWORD=secret
   export OS_PROJECT_NAME=demo
   export OS_USER_DOMAIN_NAME=Default
   export OS_PROJECT_DOMAIN_NAME=Default
   export OS_STORAGE_URL=http://rustfs:9000/v1/AUTH_project123

   swift list
   swift upload photos beach.jpg
   swift download photos beach.jpg
   ```

2. **OpenStack SDK**
   ```python
   from openstack import connection

   conn = connection.Connection(
       auth_url='http://keystone:5000/v3',
       username='admin',
       password='secret',
       project_name='demo',
       user_domain_name='Default',
       project_domain_name='Default',
       swift_endpoint_override='http://rustfs:9000'
   )

   # List containers
   containers = conn.object_store.containers()

   # Upload object
   conn.object_store.upload_object(
       container='photos',
       name='beach.jpg',
       data=open('beach.jpg', 'rb')
   )
   ```

3. **curl Commands**
   ```bash
   # Authenticate
   TOKEN=$(curl -X POST http://keystone:5000/v3/auth/tokens \
     -H "Content-Type: application/json" \
     -d '{"auth":{"identity":{"methods":["password"],"password":{"user":{"name":"admin","domain":{"name":"Default"},"password":"secret"}}},"scope":{"project":{"name":"demo","domain":{"name":"Default"}}}}}' \
     -i | grep X-Subject-Token | awk '{print $2}')

   # List containers
   curl -X GET http://rustfs:9000/v1/AUTH_project123 \
     -H "X-Auth-Token: $TOKEN"

   # Upload object
   curl -X PUT http://rustfs:9000/v1/AUTH_project123/photos/beach.jpg \
     -H "X-Auth-Token: $TOKEN" \
     -T beach.jpg

   # Download object
   curl -X GET http://rustfs:9000/v1/AUTH_project123/photos/beach.jpg \
     -H "X-Auth-Token: $TOKEN" \
     -o downloaded.jpg
   ```

---

## Key Insights for Implementation

### 1. Leverage Existing Keystone Integration

**The middleware is COMPLETE and READY for Swift!**
- ✅ `KeystoneAuthMiddleware` validates X-Auth-Token (Swift uses this)
- ✅ Stores credentials in `KEYSTONE_CREDENTIALS` task-local storage
- ✅ Extracts project_id and roles from token
- ✅ Returns 401 for invalid tokens
- **No changes needed to middleware layer**

### 2. Swift is a Translation Layer

**Swift API → Translation → S3 Backend**
- Container operations → Bucket operations
- Object operations → Object operations (minimal translation)
- Metadata headers → S3 metadata (prefix translation)
- ACLs → S3 policies (format translation)

**The heavy lifting (storage) is already done by ECStore!**

### 3. Unified Namespace is Key

**Goal:** Swift containers and S3 buckets must be the SAME.
```
User creates via Swift:  PUT /v1/AUTH_proj/photos
Backend creates bucket:  "proj:photos" or metadata-tagged "photos"
User lists via S3:       GET / → shows "photos" bucket
User uploads via S3:     PUT /photos/image.jpg
User downloads via Swift: GET /v1/AUTH_proj/photos/image.jpg → SAME object
```

**Implementation:** Use tenant prefix (`proj:container`) or metadata tagging.

### 4. Account Validation is Critical

**Security Requirement:** URL account MUST match token's project.
```rust
// Extract from URL
let url_project = account.strip_prefix("AUTH_")?;

// Extract from credentials
let token_project = credentials.claims
    .get("keystone_project_id")?;

// Verify
if url_project != token_project {
    return Err(403 Forbidden);
}
```

**This prevents cross-tenant access attacks!**

### 5. Response Format Matters

**Swift clients expect specific formats:**
- `?format=json` → JSON response
- `Accept: application/json` → JSON response
- Default → Plain text (one line per item)

**Must implement all three formats for compatibility.**

### 6. Incremental Implementation

**Phase 1 (MVP):** Container + Object CRUD
- ✅ Get 80% functionality with 20% effort
- ✅ Users can store/retrieve data
- ✅ Keystone auth working
- ⏭️ Defer: TempURL, SLO/DLO, versioning

**Phase 2:** Swift-specific features
- TempURL for CDN use cases
- Large object support for big files
- Versioning for compliance

**Phase 3:** Advanced features
- Swift ACLs
- Object expiration
- Container sync

---

## Reference Checklist for Developers

### When Implementing Swift Handlers

**For EVERY Swift handler function, check:**

- [ ] Does it call `validate_account_access(account, credentials)`?
- [ ] Does it use `KEYSTONE_CREDENTIALS.try_with()` to get credentials?
- [ ] Does it translate Swift container → S3 bucket correctly?
- [ ] Does it handle all response formats (JSON/XML/plain)?
- [ ] Does it set Swift-specific response headers (X-Trans-Id, X-Timestamp)?
- [ ] Does it map Swift metadata headers to S3 metadata?
- [ ] Does it handle errors with Swift error responses?
- [ ] Does it work with both tenant-prefixed and non-prefixed buckets?
- [ ] Does it log tenant/project info for audit trails?
- [ ] Is it tested with integration tests?

### When Adding New Features

**Before implementing, answer:**

- [ ] How does Ceph RGW implement this? (Check Ceph docs)
- [ ] What OpenStack Swift API specification says? (Check Swift docs)
- [ ] Can we reuse existing S3 backend? (Prefer reuse over new code)
- [ ] Does it need new configuration? (Add to KeystoneConfig)
- [ ] Does it need new metadata storage? (Use existing metadata system)
- [ ] Is it compatible with S3 API? (Unified namespace requirement)

### Security Checklist

- [ ] All Swift endpoints validate Keystone token
- [ ] Account URLs verified against token project_id
- [ ] Tenant isolation enforced at query level
- [ ] No cross-tenant information leaks in error messages
- [ ] ACLs respect project boundaries
- [ ] TempURL signatures validated with HMAC
- [ ] Admin roles checked for administrative operations

---

## Appendix: Quick Reference

### Important Files

| File | Purpose |
|------|---------|
| `crates/keystone/src/middleware.rs` | Keystone middleware (READY FOR SWIFT) |
| `crates/keystone/src/auth.rs` | KeystoneAuthProvider |
| `rustfs/src/auth.rs` | Auth integration (get_secret_key, check_key_valid) |
| `rustfs/src/swift/router.rs` | Swift URL routing (TO CREATE) |
| `rustfs/src/swift/container.rs` | Container operations (TO CREATE) |
| `rustfs/src/swift/object.rs` | Object operations (TO CREATE) |
| `rustfs/src/swift/errors.rs` | Swift error responses (TO CREATE) |

### Key Functions

```rust
// Already implemented ✅
KeystoneAuthMiddleware::call()  // Token validation
KEYSTONE_CREDENTIALS.try_with() // Get credentials from task-local
check_key_valid()               // Role-based authorization

// To implement 🎯
validate_account_access()       // Verify URL account matches token project
swift_to_s3_bucket()            // Translate container → bucket
format_response()               // JSON/XML/plain formatting
parse_swift_acl()               // Swift ACL → S3 policy translation
```

### Critical Constants

```rust
const SWIFT_API_VERSION: &str = "v1";
const ACCOUNT_PREFIX: &str = "AUTH_";
const RESELLER_ADMIN_ROLE: &str = "reseller_admin";
const TENANT_SEPARATOR: char = ':';
const TEMP_URL_KEY_HEADER: &str = "x-account-meta-temp-url-key";
const TEMP_URL_KEY2_HEADER: &str = "x-account-meta-temp-url-key-2";
```

### Useful Regex Patterns

```rust
// Swift account URL: /v1/AUTH_{uuid}
const ACCOUNT_PATTERN: &str = r"^/v1/AUTH_([a-f0-9]{32})$";

// Swift container URL: /v1/AUTH_{uuid}/{container}
const CONTAINER_PATTERN: &str = r"^/v1/AUTH_([a-f0-9]{32})/([^/]+)$";

// Swift object URL: /v1/AUTH_{uuid}/{container}/{object}
const OBJECT_PATTERN: &str = r"^/v1/AUTH_([a-f0-9]{32})/([^/]+)/(.+)$";
```

---

## Conclusion

This reference document provides comprehensive guidance for implementing OpenStack Swift API support in RustFS with Keystone authentication. The key insight is that **the Keystone integration is already complete** — we just need to build the Swift protocol layer on top of the existing foundation.

**Success Criteria:**
- ✅ Swift API clients (python-swiftclient, OpenStack SDK) work seamlessly
- ✅ Data written via Swift is readable via S3 (unified namespace)
- ✅ Tenant isolation enforced via Keystone project scoping
- ✅ All Swift core operations supported (containers, objects, metadata)
- ✅ Performance on par with S3 API (minimal translation overhead)

**Next Steps:**
1. Review this document with the team
2. Create GitHub issue with Phase 1 tasks
3. Begin implementation with Swift router infrastructure
4. Iterate through phases with continuous testing

---

**Document Maintainers:** RustFS Core Team
**Last Updated:** 2026-02-27
**Version:** 1.0
