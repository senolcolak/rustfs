// Copyright 2024 RustFS Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Swift object operations
//!
//! This module implements Swift object CRUD operations including upload, download,
//! metadata management, and server-side copy.

use crate::swift::{SwiftError, SwiftResult};
use crate::swift::account::validate_account_access;
use crate::swift::container::ContainerMapper;
use axum::http::HeaderMap;
use rustfs_credentials::Credentials;
use rustfs_ecstore::new_object_layer_fn;
use rustfs_ecstore::store_api::{BucketOptions, ObjectIO, ObjectOptions, PutObjReader, StorageAPI};
use rustfs_rio::{HashReader, Reader, WarpReader};
use std::collections::HashMap;

/// Object key translator for Swift object names
///
/// Handles URL encoding/decoding and path normalization for Swift object keys.
/// Swift object names can contain any UTF-8 characters except null bytes.
#[allow(dead_code)] // Phase 3: Will be used in object operations
pub struct ObjectKeyMapper;

impl ObjectKeyMapper {
    /// Create a new object key mapper
    #[allow(dead_code)] // Phase 3: Will be used in object operations
    pub fn new() -> Self {
        Self
    }

    /// Validate Swift object name
    ///
    /// Object names must:
    /// - Not be empty
    /// - Not exceed 1024 bytes (UTF-8 encoded)
    /// - Not contain null bytes
    /// - Not contain '..' path segments (directory traversal)
    /// - Not start with '/' (leading slash handled by routing)
    #[allow(dead_code)] // Phase 3: Will be used in object operations
    pub fn validate_object_name(object: &str) -> SwiftResult<()> {
        if object.is_empty() {
            return Err(SwiftError::BadRequest(
                "Object name cannot be empty".to_string(),
            ));
        }

        if object.len() > 1024 {
            return Err(SwiftError::BadRequest(
                "Object name too long (max 1024 bytes)".to_string(),
            ));
        }

        if object.contains('\0') {
            return Err(SwiftError::BadRequest(
                "Object name cannot contain null bytes".to_string(),
            ));
        }

        // Check for directory traversal attempts
        if object.contains("..") {
            // Allow ".." as part of a filename, but not as a path segment
            for segment in object.split('/') {
                if segment == ".." {
                    return Err(SwiftError::BadRequest(
                        "Object name cannot contain '..' path segments".to_string(),
                    ));
                }
            }
        }

        Ok(())
    }

    /// Convert Swift object name to S3 object key
    ///
    /// Swift object names are URL-decoded when received in the URL path,
    /// then stored as-is in S3. Special characters are preserved.
    ///
    /// Example:
    /// - Swift: "photos/vacation/beach photo.jpg"
    /// - S3: "photos/vacation/beach photo.jpg"
    #[allow(dead_code)] // Phase 3: Will be used in object operations
    pub fn swift_to_s3_key(object: &str) -> SwiftResult<String> {
        Self::validate_object_name(object)?;
        Ok(object.to_string())
    }

    /// Convert S3 object key to Swift object name
    ///
    /// This is essentially an identity transformation since we store
    /// Swift object names as-is in S3.
    #[allow(dead_code)] // Phase 3: Will be used in object operations
    pub fn s3_to_swift_name(key: &str) -> String {
        key.to_string()
    }

    /// Build full S3 object key from container and object
    ///
    /// Combines container name (with tenant prefix) and object name.
    /// The container is already validated and includes tenant prefix.
    ///
    /// Example:
    /// - Container: "abc123:photos"
    /// - Object: "vacation/beach.jpg"
    /// - Bucket: "abc123:photos"
    /// - Key: "vacation/beach.jpg"
    #[allow(dead_code)] // Phase 3: Will be used in object operations
    pub fn build_s3_key(object: &str) -> SwiftResult<String> {
        Self::swift_to_s3_key(object)
    }

    /// Extract object name from URL path
    ///
    /// The object name comes from the URL path and may be percent-encoded.
    /// This function handles URL decoding while preserving special characters.
    ///
    /// Example URL: /v1/AUTH_abc/container/path%2Fto%2Ffile.txt
    /// Decoded: "path/to/file.txt"
    #[allow(dead_code)] // Phase 3: Will be used in object operations
    pub fn decode_object_from_url(encoded: &str) -> SwiftResult<String> {
        // Decode percent-encoding
        let decoded = urlencoding::decode(encoded)
            .map_err(|e| SwiftError::BadRequest(format!("Invalid URL encoding: {}", e)))?;

        Self::validate_object_name(&decoded)?;
        Ok(decoded.to_string())
    }

    /// Encode object name for URL
    ///
    /// When constructing URLs (e.g., for redirect responses), we need to
    /// percent-encode object names.
    #[allow(dead_code)] // Phase 3: Will be used in object operations
    pub fn encode_object_for_url(object: &str) -> String {
        urlencoding::encode(object).to_string()
    }

    /// Check if object name represents a directory (pseudo-directory)
    ///
    /// In Swift, objects ending with '/' are treated as directory markers.
    #[allow(dead_code)] // Phase 3: Will be used in object operations
    pub fn is_directory_marker(object: &str) -> bool {
        object.ends_with('/')
    }

    /// Normalize object path
    ///
    /// Removes redundant slashes and normalizes the path while preserving
    /// trailing slashes for directory markers.
    #[allow(dead_code)] // Phase 3: Will be used in object operations
    pub fn normalize_path(object: &str) -> String {
        // Split by '/', filter out empty segments (except if it's the end)
        let segments: Vec<&str> = object.split('/').collect();
        let has_trailing_slash = object.ends_with('/');

        let normalized_segments: Vec<&str> = segments
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();

        let mut result = normalized_segments.join("/");

        // Preserve trailing slash for directory markers
        if has_trailing_slash && !result.is_empty() {
            result.push('/');
        }

        result
    }
}

impl Default for ObjectKeyMapper {
    fn default() -> Self {
        Self::new()
    }
}

/// Upload an object to Swift storage (PUT)
///
/// Maps Swift container/object to S3 bucket/key and stores the object.
/// Extracts metadata from X-Object-Meta-* headers and returns ETag.
///
/// # Arguments
/// * `account` - Swift account name (AUTH_{project_id})
/// * `container` - Container name
/// * `object` - Object name
/// * `credentials` - User credentials with project_id
/// * `reader` - Object content reader (implements AsyncRead)
/// * `headers` - HTTP headers including metadata
///
/// # Returns
/// * `Ok(etag)` - Object ETag on success
/// * `Err(SwiftError)` - Error if validation fails or upload fails
#[allow(dead_code)] // Phase 3: Will be used in handler for PUT object operation
pub async fn put_object<R>(
    account: &str,
    container: &str,
    object: &str,
    credentials: &Credentials,
    reader: R,
    headers: &HeaderMap,
) -> SwiftResult<String>
where
    R: tokio::io::AsyncRead + Unpin + Send + Sync + 'static,
{
    // 1. Validate account access and get project_id
    let project_id = validate_account_access(account, credentials)?;

    // 2. Validate object name
    ObjectKeyMapper::validate_object_name(object)?;

    // 3. Get S3 key from object name
    let s3_key = ObjectKeyMapper::swift_to_s3_key(object)?;

    // 4. Map container to bucket using tenant prefixing
    let mapper = ContainerMapper::default();
    let bucket = mapper.swift_to_s3_bucket(container, &project_id);

    // 5. Extract Swift metadata from X-Object-Meta-* headers
    let mut user_metadata = HashMap::new();
    for (header_name, header_value) in headers.iter() {
        let header_str = header_name.as_str().to_lowercase();
        if header_str.starts_with("x-object-meta-") {
            let meta_key = &header_str[14..]; // Remove "x-object-meta-" prefix
            if let Ok(value_str) = header_value.to_str() {
                user_metadata.insert(meta_key.to_string(), value_str.to_string());
            }
        }
    }

    // 6. Extract Content-Type if provided
    if let Some(content_type) = headers.get("content-type") {
        if let Ok(ct_str) = content_type.to_str() {
            user_metadata.insert("content-type".to_string(), ct_str.to_string());
        }
    }

    // 7. Get content length from headers (-1 if not provided)
    let content_length = headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(-1);

    // 8. Get storage layer
    let Some(store) = new_object_layer_fn() else {
        return Err(SwiftError::InternalServerError(
            "Storage layer not initialized".to_string()
        ));
    };

    // 8. Verify bucket/container exists
    store
        .get_bucket_info(&bucket, &BucketOptions::default())
        .await
        .map_err(|e| {
            if e.to_string().contains("does not exist") {
                SwiftError::NotFound(format!("Container '{}' not found", container))
            } else {
                SwiftError::InternalServerError(format!(
                    "Failed to verify container: {}",
                    e
                ))
            }
        })?;

    // 9. Prepare object options with metadata
    let mut opts = ObjectOptions::default();
    opts.user_defined = user_metadata;

    // 10. Wrap reader in buffered reader then WarpReader (Box<dyn Reader>)
    let buf_reader = tokio::io::BufReader::new(reader);
    let warp_reader: Box<dyn Reader> = Box::new(WarpReader::new(buf_reader));

    // 11. Create HashReader (no MD5/SHA256 validation for Swift)
    let hash_reader = HashReader::new(
        warp_reader,
        content_length,
        content_length,
        None,  // md5hex
        None,  // sha256hex
        false, // disable_multipart
    )
    .map_err(|e| SwiftError::InternalServerError(format!("Failed to create hash reader: {}", e)))?;

    // 12. Wrap in PutObjReader as expected by storage layer
    let mut put_reader = PutObjReader::new(hash_reader);

    // 13. Upload object to storage
    let obj_info = store
        .put_object(&bucket, &s3_key, &mut put_reader, &opts)
        .await
        .map_err(|e| {
            SwiftError::InternalServerError(format!("Failed to upload object: {}", e))
        })?;

    // 14. Return ETag (MD5 hash in hex format)
    Ok(obj_info.etag.unwrap_or_default())
}

/// Download an object from Swift storage (GET)
///
/// Maps Swift container/object to S3 bucket/key and retrieves the object content.
/// Returns the object stream and metadata.
///
/// # Arguments
/// * `account` - Swift account name (AUTH_{project_id})
/// * `container` - Container name
/// * `object` - Object name
/// * `credentials` - User credentials with project_id
///
/// # Returns
/// * `Ok((stream, object_info))` - Object content stream and metadata
/// * `Err(SwiftError)` - Error if validation fails or object not found
#[allow(dead_code)] // Phase 3: Will be used in handler for GET object operation
pub async fn get_object(
    account: &str,
    container: &str,
    object: &str,
    credentials: &Credentials,
) -> SwiftResult<rustfs_ecstore::store_api::GetObjectReader> {
    use rustfs_ecstore::store_api::GetObjectReader;

    // 1. Validate account access and get project_id
    let project_id = validate_account_access(account, credentials)?;

    // 2. Validate object name
    ObjectKeyMapper::validate_object_name(object)?;

    // 3. Get S3 key from object name
    let s3_key = ObjectKeyMapper::swift_to_s3_key(object)?;

    // 4. Map container to bucket using tenant prefixing
    let mapper = ContainerMapper::default();
    let bucket = mapper.swift_to_s3_bucket(container, &project_id);

    // 5. Get storage layer
    let Some(store) = new_object_layer_fn() else {
        return Err(SwiftError::InternalServerError(
            "Storage layer not initialized".to_string()
        ));
    };

    // 6. Prepare object options
    let opts = ObjectOptions::default();

    // 7. Get object reader from storage
    let reader: GetObjectReader = store
        .get_object_reader(&bucket, &s3_key, None, HeaderMap::new(), &opts)
        .await
        .map_err(|e| {
            let err_str = e.to_string();
            if err_str.contains("does not exist") || err_str.contains("not found") {
                SwiftError::NotFound(format!("Object '{}' not found in container '{}'", object, container))
            } else {
                SwiftError::InternalServerError(format!("Failed to read object: {}", e))
            }
        })?;

    Ok(reader)
}

/// Get object metadata without content (HEAD)
///
/// Maps Swift container/object to S3 bucket/key and retrieves only the metadata.
/// This is more efficient than GET when only metadata is needed.
///
/// # Arguments
/// * `account` - Swift account name (AUTH_{project_id})
/// * `container` - Container name
/// * `object` - Object name
/// * `credentials` - User credentials with project_id
///
/// # Returns
/// * `Ok(object_info)` - Object metadata (ObjectInfo)
/// * `Err(SwiftError)` - Error if validation fails or object not found
#[allow(dead_code)] // Phase 3: Will be used in handler for HEAD object operation
pub async fn head_object(
    account: &str,
    container: &str,
    object: &str,
    credentials: &Credentials,
) -> SwiftResult<rustfs_ecstore::store_api::ObjectInfo> {
    use rustfs_ecstore::store_api::ObjectInfo;

    // 1. Validate account access and get project_id
    let project_id = validate_account_access(account, credentials)?;

    // 2. Validate object name
    ObjectKeyMapper::validate_object_name(object)?;

    // 3. Get S3 key from object name
    let s3_key = ObjectKeyMapper::swift_to_s3_key(object)?;

    // 4. Map container to bucket using tenant prefixing
    let mapper = ContainerMapper::default();
    let bucket = mapper.swift_to_s3_bucket(container, &project_id);

    // 5. Get storage layer
    let Some(store) = new_object_layer_fn() else {
        return Err(SwiftError::InternalServerError(
            "Storage layer not initialized".to_string()
        ));
    };

    // 6. Prepare object options
    let opts = ObjectOptions::default();

    // 7. Get object info (metadata only) from storage
    let info: ObjectInfo = store
        .get_object_info(&bucket, &s3_key, &opts)
        .await
        .map_err(|e| {
            let err_str = e.to_string();
            if err_str.contains("does not exist") || err_str.contains("not found") {
                SwiftError::NotFound(format!("Object '{}' not found in container '{}'", object, container))
            } else {
                SwiftError::InternalServerError(format!("Failed to get object metadata: {}", e))
            }
        })?;

    // 8. Check if this is a delete marker
    if info.delete_marker {
        return Err(SwiftError::NotFound(format!("Object '{}' not found in container '{}'", object, container)));
    }

    Ok(info)
}

/// Delete an object from Swift storage (DELETE)
///
/// Maps Swift container/object to S3 bucket/key and deletes the object.
/// Swift DELETE is idempotent - deleting a non-existent object returns success.
///
/// # Arguments
/// * `account` - Swift account name (AUTH_{project_id})
/// * `container` - Container name
/// * `object` - Object name
/// * `credentials` - User credentials with project_id
///
/// # Returns
/// * `Ok(())` - Object deleted successfully (or didn't exist)
/// * `Err(SwiftError)` - Error if validation fails or deletion fails
#[allow(dead_code)] // Phase 3: Will be used in handler for DELETE object operation
pub async fn delete_object(
    account: &str,
    container: &str,
    object: &str,
    credentials: &Credentials,
) -> SwiftResult<()> {
    // 1. Validate account access and get project_id
    let project_id = validate_account_access(account, credentials)?;

    // 2. Validate object name
    ObjectKeyMapper::validate_object_name(object)?;

    // 3. Get S3 key from object name
    let s3_key = ObjectKeyMapper::swift_to_s3_key(object)?;

    // 4. Map container to bucket using tenant prefixing
    let mapper = ContainerMapper::default();
    let bucket = mapper.swift_to_s3_bucket(container, &project_id);

    // 5. Get storage layer
    let Some(store) = new_object_layer_fn() else {
        return Err(SwiftError::InternalServerError(
            "Storage layer not initialized".to_string()
        ));
    };

    // 6. Prepare object options for deletion
    let opts = ObjectOptions::default();

    // 7. Delete object from storage
    // Swift DELETE is idempotent - returns success even if object doesn't exist
    let _result = store
        .delete_object(&bucket, &s3_key, opts)
        .await
        .map_err(|e| {
            let err_str = e.to_string();
            if err_str.contains("Bucket not found") || err_str.contains("does not exist") {
                SwiftError::NotFound(format!("Container '{}' not found", container))
            } else {
                SwiftError::InternalServerError(format!("Failed to delete object: {}", e))
            }
        })?;

    // 8. Swift DELETE is idempotent - always return success
    Ok(())
}

/// Update object metadata (POST)
///
/// Updates user-defined metadata (X-Object-Meta-*) for an existing object
/// without changing the object content. This is a Swift-specific operation.
///
/// # Arguments
/// * `account` - Swift account name (AUTH_{project_id})
/// * `container` - Container name
/// * `object` - Object name
/// * `credentials` - User credentials with project_id
/// * `headers` - HTTP headers containing X-Object-Meta-* headers to update
///
/// # Returns
/// * `Ok(())` - Metadata updated successfully
/// * `Err(SwiftError)` - Error if validation fails, object not found, or update fails
#[allow(dead_code)] // Phase 3: Will be used in handler for POST object operation
pub async fn update_object_metadata(
    account: &str,
    container: &str,
    object: &str,
    credentials: &Credentials,
    headers: &HeaderMap,
) -> SwiftResult<()> {
    // 1. Validate account access and get project_id
    let project_id = validate_account_access(account, credentials)?;

    // 2. Validate object name
    ObjectKeyMapper::validate_object_name(object)?;

    // 3. Get S3 key from object name
    let s3_key = ObjectKeyMapper::swift_to_s3_key(object)?;

    // 4. Map container to bucket using tenant prefixing
    let mapper = ContainerMapper::default();
    let bucket = mapper.swift_to_s3_bucket(container, &project_id);

    // 5. Get storage layer
    let Some(store) = new_object_layer_fn() else {
        return Err(SwiftError::InternalServerError(
            "Storage layer not initialized".to_string()
        ));
    };

    // 6. First, get the existing object info to verify it exists
    let opts = ObjectOptions::default();
    let existing_info = store
        .get_object_info(&bucket, &s3_key, &opts)
        .await
        .map_err(|e| {
            let err_str = e.to_string();
            if err_str.contains("does not exist") || err_str.contains("not found") {
                SwiftError::NotFound(format!("Object '{}' not found in container '{}'", object, container))
            } else {
                SwiftError::InternalServerError(format!("Failed to get object info: {}", e))
            }
        })?;

    // 7. Check if this is a delete marker
    if existing_info.delete_marker {
        return Err(SwiftError::NotFound(format!("Object '{}' not found in container '{}'", object, container)));
    }

    // 8. Extract new metadata from X-Object-Meta-* headers
    let mut new_metadata = HashMap::new();
    for (header_name, header_value) in headers.iter() {
        let header_str = header_name.as_str().to_lowercase();
        if header_str.starts_with("x-object-meta-") {
            let meta_key = &header_str[14..]; // Remove "x-object-meta-" prefix
            if let Ok(value_str) = header_value.to_str() {
                new_metadata.insert(meta_key.to_string(), value_str.to_string());
            }
        }
    }

    // 9. Also update Content-Type if provided
    if let Some(content_type) = headers.get("content-type") {
        if let Ok(ct_str) = content_type.to_str() {
            new_metadata.insert("content-type".to_string(), ct_str.to_string());
        }
    }

    // 10. Prepare options for metadata update
    // Swift POST replaces all custom metadata, not merges
    let mut update_opts = ObjectOptions::default();
    update_opts.user_defined = new_metadata;
    update_opts.mod_time = existing_info.mod_time; // Preserve modification time
    update_opts.version_id = existing_info.version_id.map(|v| v.to_string()); // Preserve version

    // 11. Update object metadata
    let _updated_info = store
        .put_object_metadata(&bucket, &s3_key, &update_opts)
        .await
        .map_err(|e| {
            SwiftError::InternalServerError(format!("Failed to update object metadata: {}", e))
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_object_name_valid() {
        assert!(ObjectKeyMapper::validate_object_name("myfile.txt").is_ok());
        assert!(ObjectKeyMapper::validate_object_name("path/to/file.jpg").is_ok());
        assert!(ObjectKeyMapper::validate_object_name("file with spaces.pdf").is_ok());
        assert!(ObjectKeyMapper::validate_object_name("special-chars_@#$.txt").is_ok());
        assert!(ObjectKeyMapper::validate_object_name("unicode-文件.txt").is_ok());
    }

    #[test]
    fn test_validate_object_name_empty() {
        let result = ObjectKeyMapper::validate_object_name("");
        assert!(result.is_err());
        match result {
            Err(SwiftError::BadRequest(msg)) => {
                assert!(msg.contains("empty"));
            }
            _ => panic!("Expected BadRequest error"),
        }
    }

    #[test]
    fn test_validate_object_name_too_long() {
        let long_name = "a".repeat(1025);
        let result = ObjectKeyMapper::validate_object_name(&long_name);
        assert!(result.is_err());
        match result {
            Err(SwiftError::BadRequest(msg)) => {
                assert!(msg.contains("too long"));
            }
            _ => panic!("Expected BadRequest error"),
        }
    }

    #[test]
    fn test_validate_object_name_null_byte() {
        let result = ObjectKeyMapper::validate_object_name("file\0name.txt");
        assert!(result.is_err());
        match result {
            Err(SwiftError::BadRequest(msg)) => {
                assert!(msg.contains("null"));
            }
            _ => panic!("Expected BadRequest error"),
        }
    }

    #[test]
    fn test_validate_object_name_directory_traversal() {
        // ".." as a path segment should be rejected
        assert!(ObjectKeyMapper::validate_object_name("path/../file.txt").is_err());
        assert!(ObjectKeyMapper::validate_object_name("../file.txt").is_err());
        assert!(ObjectKeyMapper::validate_object_name("path/..").is_err());

        // But ".." in a filename should be allowed
        assert!(ObjectKeyMapper::validate_object_name("file..txt").is_ok());
        assert!(ObjectKeyMapper::validate_object_name("my..file.txt").is_ok());
    }

    #[test]
    fn test_swift_to_s3_key() {
        assert_eq!(
            ObjectKeyMapper::swift_to_s3_key("file.txt").unwrap(),
            "file.txt"
        );
        assert_eq!(
            ObjectKeyMapper::swift_to_s3_key("path/to/file.jpg").unwrap(),
            "path/to/file.jpg"
        );
        assert_eq!(
            ObjectKeyMapper::swift_to_s3_key("file with spaces.pdf").unwrap(),
            "file with spaces.pdf"
        );
    }

    #[test]
    fn test_s3_to_swift_name() {
        assert_eq!(
            ObjectKeyMapper::s3_to_swift_name("file.txt"),
            "file.txt"
        );
        assert_eq!(
            ObjectKeyMapper::s3_to_swift_name("path/to/file.jpg"),
            "path/to/file.jpg"
        );
    }

    #[test]
    fn test_decode_object_from_url() {
        // Basic decoding
        assert_eq!(
            ObjectKeyMapper::decode_object_from_url("file.txt").unwrap(),
            "file.txt"
        );

        // Percent-encoded spaces
        assert_eq!(
            ObjectKeyMapper::decode_object_from_url("file%20with%20spaces.txt").unwrap(),
            "file with spaces.txt"
        );

        // Percent-encoded special characters
        assert_eq!(
            ObjectKeyMapper::decode_object_from_url("path%2Fto%2Ffile.txt").unwrap(),
            "path/to/file.txt"
        );

        // Unicode characters
        assert_eq!(
            ObjectKeyMapper::decode_object_from_url("%E6%96%87%E4%BB%B6.txt").unwrap(),
            "文件.txt"
        );
    }

    #[test]
    fn test_encode_object_for_url() {
        assert_eq!(
            ObjectKeyMapper::encode_object_for_url("file.txt"),
            "file.txt"
        );

        assert_eq!(
            ObjectKeyMapper::encode_object_for_url("file with spaces.txt"),
            "file%20with%20spaces.txt"
        );

        assert_eq!(
            ObjectKeyMapper::encode_object_for_url("path/to/file.txt"),
            "path%2Fto%2Ffile.txt"
        );
    }

    #[test]
    fn test_is_directory_marker() {
        assert!(ObjectKeyMapper::is_directory_marker("folder/"));
        assert!(ObjectKeyMapper::is_directory_marker("path/to/dir/"));
        assert!(!ObjectKeyMapper::is_directory_marker("file.txt"));
        assert!(!ObjectKeyMapper::is_directory_marker("folder"));
    }

    #[test]
    fn test_normalize_path() {
        // Remove redundant slashes
        assert_eq!(
            ObjectKeyMapper::normalize_path("path//to///file.txt"),
            "path/to/file.txt"
        );

        // Preserve trailing slash for directories
        assert_eq!(
            ObjectKeyMapper::normalize_path("path/to/dir/"),
            "path/to/dir/"
        );

        // Remove leading/trailing slashes except for directory marker
        assert_eq!(
            ObjectKeyMapper::normalize_path("/path/to/file.txt"),
            "path/to/file.txt"
        );

        // Empty segments
        assert_eq!(
            ObjectKeyMapper::normalize_path("path/./to/file.txt"),
            "path/./to/file.txt"  // We keep "." as it might be intentional
        );
    }

    #[test]
    fn test_build_s3_key() {
        assert_eq!(
            ObjectKeyMapper::build_s3_key("file.txt").unwrap(),
            "file.txt"
        );

        assert_eq!(
            ObjectKeyMapper::build_s3_key("path/to/file.jpg").unwrap(),
            "path/to/file.jpg"
        );
    }
}
