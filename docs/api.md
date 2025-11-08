| Operation                                    | HTTP Method                                 | URL pattern / action                             | Description                                                                                                                                           |
| -------------------------------------------- | ------------------------------------------- | ------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| Upload object                                | `PUT`                                       | `/{bucket}/{*key}`                               | Upload/create a new object (file) in the bucket. Example in AWS: single `PutObject`. ([Dokumen AWS][3])                                               |
| Download/get object                          | `GET`                                       | `/{bucket}/{*key}`                               | Retrieve the object. Typically returns the file contents plus metadata.                                                                               |
| List objects                                 | `GET`                                       | `/{bucket}?prefix={…}&delimiter={…}` etc         | List objects (“keys”) in a bucket (often optionally filtered by prefix). See “Organizing, listing, and working with your objects”. ([Dokumen AWS][4]) |
| Delete object                                | `DELETE`                                    | `/{bucket}/{*key}`                               | Remove/delete an object from the bucket.                                                                                                              |
| Head object (get metadata only)              | `HEAD`                                      | `/{bucket}/{*key}`                               | Retrieve metadata (size, timestamp, ETag, etc) without fetching full content.                                                                         |
| Multipart upload (for large files)           | `POST` or `PUT` sub-calls                   | Initiate + UploadPart + CompleteMultipartUpload  | For large objects you break into parts. ([Dokumen AWS][5])                                                                                            |
| Create bucket / delete bucket / list buckets | `PUT` / `DELETE` / `GET` on bucket resource | Manage buckets (containers) rather than objects. |                                                                                                                                                       |

### 1. Upload Object (`PUT /{bucket}/{*key}`)

**Request**

* Method: `PUT`
* URL: `/bucketName/path/to/object.ext`
* Headers:

  ```http
  Content-Type: application/octet-stream
  x-amz-metadata-foo: bar
  Authorization: <signature>
  ```
* Body: raw bytes of the file content (not JSON)

**Response**

```json
{
  "ETag": "\"6805f2cfc46c0f04559748bb039d69ae\"",
  "VersionId": "3/L4kqtJlcpXroDTDmJ+rmSpXd3…",
  "RequestId": "4442587FB7D0A2F9",
  "HostId": "HostIdExample/uyv+7KqLWjP1…"
}
```

---

### 2. Download/Get Object (`GET /{bucket}/{*key}`)

**Request**

* Method: `GET`
* URL: `/bucketName/path/to/object.ext`
* Headers:

  ```http
  Authorization: <signature>
  ```

**Response**

* Status: `200 OK`
* Headers:

  ```
  Content-Type: application/octet-stream
  Content-Length: 12345
  ETag: "6805f2cfc46c0f04559748bb039d69ae"
  Last-Modified: Wed, 21 Oct 2020 07:28:00 GMT
  x-amz-metadata-foo: bar
  ```
* Body: raw bytes of file content

**If metadata only (HEAD request)**

```json
{
  "ContentLength": 12345,
  "ContentType": "application/octet-stream",
  "ETag": "\"6805f2cfc46c0f04559748bb039d69ae\"",
  "LastModified": "2020-10-21T07:28:00Z",
  "Metadata": {
    "foo": "bar"
  }
}
```

---

### 3. List Objects (`GET /{bucket}?prefix=…&delimiter=…&max-keys=…`)

**Request**

* Method: `GET`
* URL: `/bucketName?prefix=images/&delimiter=/&max-keys=100`
* Headers:

  ```http
  Authorization: <signature>
  ```

**Response**

```json
{
  "IsTruncated": false,
  "Contents": [
    {
      "Key": "images/photo1.jpg",
      "LastModified": "2025-11-08T12:34:56Z",
      "ETag": "\"9c8af9a76df052144598c115ef33e511\"",
      "Size": 102400,
      "StorageClass": "STANDARD"
    },
    {
      "Key": "images/photo2.jpg",
      "LastModified": "2025-11-07T11:22:33Z",
      "ETag": "\"1b2cf535f27731c974343645a3985328\"",
      "Size": 204800,
      "StorageClass": "STANDARD"
    }
  ],
  "Name": "bucketName",
  "Prefix": "images/",
  "Delimiter": "/",
  "MaxKeys": 100,
  "CommonPrefixes": [
    {
      "Prefix": "images/archive/"
    }
  ],
  "KeyCount": 2
}
```

---

### 4. Delete Object (`DELETE /{bucket}/{*key}`)

**Request**

* Method: `DELETE`
* URL: `/bucketName/path/to/object.ext`
* Headers:

  ```http
  Authorization: <signature>
  ```

**Response**

```json
{
  "DeleteMarker": true,
  "VersionId": "3/L4kqtJlcpXroDTDmJ+rmSpXd3…",
  "RequestId": "4442587FB7D0A2F9",
  "HostId": "HostIdExample/uyv+7KqLWjP1…"
}
```

---

### 5. Initiate Multipart Upload (`POST /{bucket}/{*key}?uploads`)

**Request**

* Method: `POST`
* URL: `/bucketName/largefile.zip?uploads`
* Headers:

  ```http
  Authorization: <signature>
  ```
* Body: (optional JSON with metadata/custom settings)

```json
{
  "ContentType": "application/zip",
  "Metadata": {
    "foo": "bar"
  }
}
```

**Response**

```json
{
  "Bucket": "bucketName",
  "Key": "largefile.zip",
  "UploadId": "XKJH235jkfSDFKJ345",
  "RequestId": "4442587FB7D0A2F9",
  "HostId": "HostIdExample/uyv+7KqLWjP1…"
}
```

---

### 6. Upload Part (`PUT /{bucket}/{*key}?partNumber={n}&uploadId={id}`)

**Request**

* Method: `PUT`
* URL: `/bucketName/largefile.zip?partNumber=1&uploadId=XKJH235jkfSDFKJ345`
* Headers:

  ```http
  Authorization: <signature>
  Content-Length: 5242880
  Content-MD5: <base64-md5-of-part>
  ```
* Body: raw bytes of part content

**Response**

```json
{
  "ETag": "\"9c8af9a76df052144598c115ef33e511\"",
  "RequestId": "4442587FB7D0A2F9",
  "HostId": "HostIdExample/uyv+7KqLWjP1…"
}
```

---

### 7. Complete Multipart Upload (`POST /{bucket}/{*key}?uploadId={id}`)

**Request**

* Method: `POST`
* URL: `/bucketName/largefile.zip?uploadId=XKJH235jkfSDFKJ345`
* Headers:

  ```http
  Authorization: <signature>
  Content-Type: application/json
  ```
* Body:

```json
{
  "Parts": [
    { "PartNumber": 1, "ETag": "\"9c8af9a76df052144598c115ef33e511\"" },
    { "PartNumber": 2, "ETag": "\"1b2cf535f27731c974343645a3985328\"" }
  ]
}
```

**Response**

```json
{
  "Bucket": "bucketName",
  "Key": "largefile.zip",
  "ETag": "\"d8c2eafd90c266e19ab9f5b4a59d600f\"",
  "VersionId": "3/L4kqtJlcpXroDTDmJ+rmSpXd3…",
  "RequestId": "4442587FB7D0A2F9",
  "HostId": "HostIdExample/uyv+7KqLWjP1…"
}
```

---

### 8. Create Bucket (`PUT /{bucket}`)

**Request**

* Method: `PUT`
* URL: `/bucketName`
* Headers:

  ```http
  Authorization: <signature>
  Content-Type: application/json
  ```
* Body (optional):

```json
{
  "LocationConstraint": "us-west-2"
}
```

**Response**

```json
{
  "Location": "/bucketName",
  "RequestId": "4442587FB7D0A2F9",
  "HostId": "HostIdExample/uyv+7KqLWjP1…"
}
```

---

### Notes & caveats

* The real AWS S3 REST API responds in **XML**, not JSON, for many operations. ([Stack Overflow][1])
* If you’re building your own S3-compatible service (or an internal facade) you *can* define JSON payloads as above, but you’ll need to ensure clients know the format.
* Always include proper authentication (e.g., AWS Signature v4) and permission checks. ([Cloudian][2])
* Metadata, ACLs, versioning, encryption, and region/endpoint handling add complexity beyond the simple examples above.



[1]: https://stackoverflow.com/questions/9153552/amazon-s3-response-in-json?utm_source=chatgpt.com "Amazon S3 response in JSON? - Stack Overflow"
[2]: https://cloudian.com/blog/s3-api-actions-authentication-and-code-examples/?utm_source=chatgpt.com "S3 API: Common Actions, Examples, and Quick Tutorial - Cloudian"
[3]: https://docs.aws.amazon.com/AmazonS3/latest/userguide/upload-objects.html?utm_source=chatgpt.com "Uploading objects - Amazon Simple Storage Service"
[4]: https://docs.aws.amazon.com/AmazonS3/latest/userguide/uploading-downloading-objects.html?utm_source=chatgpt.com "Working with objects in Amazon S3"
[5]: https://docs.aws.amazon.com/AmazonS3/latest/userguide/mpuoverview.html?utm_source=chatgpt.com "Uploading and copying objects using multipart upload in ..."
