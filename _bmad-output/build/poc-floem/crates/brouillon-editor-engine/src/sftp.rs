use std::path::PathBuf;
use std::fs;
use std::io;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SftpConfig {
    pub host: String,
    pub user: String,
    pub port: u16,
}

pub struct SftpClient {
    base_dir: PathBuf,
    config: Option<SftpConfig>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteFile {
    pub name: String,
    pub path: String, // slash-separated relative path, e.g. "/index.html"
    pub is_dir: bool,
    pub size: u64,
}

impl Default for SftpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl SftpClient {
    pub fn new() -> Self {
        // Resolve path to sample/sftp
        let base_dir = PathBuf::from("sample/sftp");
        Self {
            base_dir,
            config: None,
        }
    }

    pub fn connect(&mut self, host: &str, user: &str, _pass: &str, port: u16) -> io::Result<()> {
        if host.is_empty() || user.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Host and user cannot be empty"));
        }
        self.config = Some(SftpConfig {
            host: host.to_string(),
            user: user.to_string(),
            port,
        });

        // Initialize directory structure if not present
        if !self.base_dir.exists() {
            fs::create_dir_all(&self.base_dir)?;
            fs::write(self.base_dir.join("index.html"), "<html>\n  <head>\n    <link rel=\"stylesheet\" href=\"style.css\">\n  </head>\n  <body>\n    <h1>Hello Remote SFTP</h1>\n  </body>\n</html>")?;
            fs::write(self.base_dir.join("style.css"), "body {\n  background-color: #1e1e1e;\n  color: #ffffff;\n}")?;
            fs::write(self.base_dir.join("script.js"), "console.log(\"Hello from SFTP!\");")?;
            
            let subdir = self.base_dir.join("subdir");
            fs::create_dir_all(&subdir)?;
            fs::write(subdir.join("config.json"), "{\n  \"port\": 8080,\n  \"debug\": true\n}")?;
        }
        Ok(())
    }

    pub fn disconnect(&mut self) {
        self.config = None;
    }

    pub fn is_connected(&self) -> bool {
        self.config.is_some()
    }

    pub fn get_config(&self) -> Option<&SftpConfig> {
        self.config.as_ref()
    }

    fn to_local_path(&self, remote_path: &str) -> PathBuf {
        let clean_path = remote_path.trim_start_matches('/');
        self.base_dir.join(clean_path)
    }

    pub fn list_dir(&self, remote_path: &str) -> io::Result<Vec<RemoteFile>> {
        if !self.is_connected() {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Not connected to SFTP server"));
        }
        let local_path = self.to_local_path(remote_path);
        if !local_path.exists() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("Remote path {} does not exist", remote_path)));
        }
        let mut files = Vec::new();
        for entry in fs::read_dir(local_path)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = entry.metadata()?;
            let is_dir = metadata.is_dir();
            let name = entry.file_name().to_string_lossy().into_owned();
            
            let relative_to_base = path.strip_prefix(&self.base_dir)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
            let relative_str = relative_to_base.to_string_lossy().replace('\\', "/");
            let remote_file_path = format!("/{}", relative_str);

            files.push(RemoteFile {
                name,
                path: remote_file_path,
                is_dir,
                size: metadata.len(),
            });
        }
        
        files.sort_by(|a, b| {
            if a.is_dir != b.is_dir {
                b.is_dir.cmp(&a.is_dir)
            } else {
                a.name.cmp(&b.name)
            }
        });
        Ok(files)
    }

    pub fn read_file(&self, remote_path: &str) -> io::Result<String> {
        if !self.is_connected() {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Not connected to SFTP server"));
        }
        let local_path = self.to_local_path(remote_path);
        fs::read_to_string(local_path)
    }

    pub fn write_file(&self, remote_path: &str, content: &str) -> io::Result<()> {
        if !self.is_connected() {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Not connected to SFTP server"));
        }
        let local_path = self.to_local_path(remote_path);
        if let Some(parent) = local_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(local_path, content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sftp_client_flow() {
        let mut client = SftpClient::new();
        // Set unique base dir for testing so we don't interfere with real files
        let test_dir = std::env::temp_dir().join(format!("brouillon-sftp-test-{}", std::process::id()));
        client.base_dir = test_dir.clone();

        assert!(!client.is_connected());
        assert!(client.list_dir("/").is_err());

        client.connect("example.com", "alice", "secret", 22).unwrap();
        assert!(client.is_connected());

        // Listing should generate sample files and succeed
        let files = client.list_dir("/").unwrap();
        assert!(!files.is_empty());

        let index_file = files.iter().find(|f| f.name == "index.html").unwrap();
        assert!(!index_file.is_dir);

        let content = client.read_file("/index.html").unwrap();
        assert!(content.contains("Hello Remote SFTP"));

        // Write a file
        client.write_file("/new_file.txt", "remote contents").unwrap();
        let new_content = client.read_file("/new_file.txt").unwrap();
        assert_eq!(new_content, "remote contents");

        client.disconnect();
        assert!(!client.is_connected());

        let _ = fs::remove_dir_all(&test_dir);
    }
}
