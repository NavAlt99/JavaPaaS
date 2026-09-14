package main

import (
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"fmt"
	"log"
	"regexp"
	"strings"
	"sync"
	"time"
)

var identifierRegex = regexp.MustCompile(`[^a-zA-Z0-9_]`)

// DBaaSManager manages the provisioning, lifecycle, and credential injection
// of dedicated PostgreSQL databases for PaaS tenants.
type DBaaSManager struct {
	adminDB    *sql.DB
	publicHost string
	publicPort int
	mu         sync.RWMutex
	store      map[string]*DatabaseInfo
}

func NewDBaaSManager(adminDB *sql.DB, publicHost string, publicPort int) *DBaaSManager {
	if publicHost == "" {
		publicHost = "127.0.0.1"
	}
	if publicPort <= 0 {
		publicPort = 5432
	}

	mgr := &DBaaSManager{
		adminDB:    adminDB,
		publicHost: publicHost,
		publicPort: publicPort,
		store:      make(map[string]*DatabaseInfo),
	}

	if adminDB != nil {
		if err := mgr.initSchema(); err != nil {
			log.Printf("Warning: failed to init DBaaS schema: %v", err)
		} else {
			mgr.loadFromDB()
		}
	}

	return mgr
}

func (m *DBaaSManager) initSchema() error {
	query := `
	CREATE TABLE IF NOT EXISTS javapaas_databases (
		tenant_id VARCHAR(128) PRIMARY KEY,
		database_name VARCHAR(128) NOT NULL,
		username VARCHAR(128) NOT NULL,
		password VARCHAR(128) NOT NULL,
		host VARCHAR(256) NOT NULL,
		port INT NOT NULL,
		jdbc_url TEXT NOT NULL,
		status VARCHAR(32) DEFAULT 'ready',
		created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
	);`
	_, err := m.adminDB.Exec(query)
	return err
}

func (m *DBaaSManager) loadFromDB() {
	query := `SELECT tenant_id, database_name, username, password, host, port, jdbc_url, status, created_at FROM javapaas_databases`
	rows, err := m.adminDB.Query(query)
	if err != nil {
		log.Printf("Warning: failed to load existing databases from postgres: %v", err)
		return
	}
	defer rows.Close()

	m.mu.Lock()
	defer m.mu.Unlock()

	for rows.Next() {
		var info DatabaseInfo
		var createdAt time.Time
		if err := rows.Scan(
			&info.TenantID,
			&info.Database,
			&info.Username,
			&info.Password,
			&info.Host,
			&info.Port,
			&info.JdbcURL,
			&info.Status,
			&createdAt,
		); err == nil {
			info.CreatedAt = createdAt.Format(time.RFC3339)
			m.store[info.TenantID] = &info
		}
	}
	log.Printf("Loaded %d managed database(s) from DBaaS store", len(m.store))
}

func generateSecurePassword(length int) string {
	bytes := make([]byte, length/2)
	if _, err := rand.Read(bytes); err != nil {
		return "paas_sec_" + fmt.Sprint(time.Now().UnixNano())
	}
	return hex.EncodeToString(bytes)
}

func sanitizeIdentifier(s string) string {
	clean := identifierRegex.ReplaceAllString(s, "_")
	clean = strings.ToLower(clean)
	if len(clean) > 40 {
		clean = clean[:40]
	}
	return clean
}

func (m *DBaaSManager) Provision(tenantID string) (*DatabaseInfo, error) {
	if strings.TrimSpace(tenantID) == "" {
		return nil, fmt.Errorf("tenant_id cannot be empty")
	}

	m.mu.Lock()
	defer m.mu.Unlock()

	// Idempotency: return existing database if already provisioned
	if existing, ok := m.store[tenantID]; ok {
		return existing, nil
	}

	cleanID := sanitizeIdentifier(tenantID)
	dbName := fmt.Sprintf("tenant_%s_db", cleanID)
	dbUser := fmt.Sprintf("tenant_%s_usr", cleanID)
	dbPass := "sec_" + generateSecurePassword(24)

	// If real PostgreSQL admin connection is active, provision on engine
	if m.adminDB != nil {
		// 1. Create dedicated user
		createUserSQL := fmt.Sprintf("CREATE USER %s WITH ENCRYPTED PASSWORD '%s';", dbUser, dbPass)
		if _, err := m.adminDB.Exec(createUserSQL); err != nil {
			// If role exists, update password
			alterUserSQL := fmt.Sprintf("ALTER USER %s WITH ENCRYPTED PASSWORD '%s';", dbUser, dbPass)
			if _, alterErr := m.adminDB.Exec(alterUserSQL); alterErr != nil {
				return nil, fmt.Errorf("create/alter postgres user '%s': %w", dbUser, err)
			}
		}

		// 2. Create isolated database
		createDBSQL := fmt.Sprintf("CREATE DATABASE %s OWNER %s;", dbName, dbUser)
		if _, err := m.adminDB.Exec(createDBSQL); err != nil {
			if !strings.Contains(err.Error(), "already exists") {
				return nil, fmt.Errorf("create postgres database '%s': %w", dbName, err)
			}
		}

		// 3. Grant privileges
		grantSQL := fmt.Sprintf("GRANT ALL PRIVILEGES ON DATABASE %s TO %s;", dbName, dbUser)
		_, _ = m.adminDB.Exec(grantSQL)
	}

	jdbcURL := fmt.Sprintf("jdbc:postgresql://%s:%d/%s?sslmode=disable", m.publicHost, m.publicPort, dbName)

	info := &DatabaseInfo{
		TenantID:  tenantID,
		Database:  dbName,
		Username:  dbUser,
		Password:  dbPass,
		Host:      m.publicHost,
		Port:      m.publicPort,
		JdbcURL:   jdbcURL,
		Status:    "ready",
		CreatedAt: time.Now().UTC().Format(time.RFC3339),
	}

	m.store[tenantID] = info

	// Persist in metadata table if admin DB is available
	if m.adminDB != nil {
		query := `
		INSERT INTO javapaas_databases (tenant_id, database_name, username, password, host, port, jdbc_url, status, created_at)
		VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())
		ON CONFLICT (tenant_id) DO UPDATE SET
			password = EXCLUDED.password,
			status = EXCLUDED.status;`
		_, _ = m.adminDB.Exec(query, info.TenantID, info.Database, info.Username, info.Password, info.Host, info.Port, info.JdbcURL, info.Status)
	}

	log.Printf("Provisioned DBaaS PostgreSQL database '%s' for tenant %s (host: %s:%d)",
		dbName, tenantID, m.publicHost, m.publicPort)
	return info, nil
}

func (m *DBaaSManager) Get(tenantID string) (*DatabaseInfo, bool) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	info, ok := m.store[tenantID]
	if !ok {
		return nil, false
	}
	copy := *info
	return &copy, true
}

func (m *DBaaSManager) List() []*DatabaseInfo {
	m.mu.RLock()
	defer m.mu.RUnlock()
	list := make([]*DatabaseInfo, 0, len(m.store))
	for _, info := range m.store {
		copy := *info
		list = append(list, &copy)
	}
	return list
}

func (m *DBaaSManager) Deprovision(tenantID string) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	info, ok := m.store[tenantID]
	if !ok {
		return fmt.Errorf("no database found for tenant %s", tenantID)
	}

	if m.adminDB != nil {
		// Terminate active connections
		termSQL := fmt.Sprintf(
			"SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '%s';",
			info.Database,
		)
		_, _ = m.adminDB.Exec(termSQL)

		// Drop database
		dropDBSQL := fmt.Sprintf("DROP DATABASE IF EXISTS %s;", info.Database)
		if _, err := m.adminDB.Exec(dropDBSQL); err != nil {
			log.Printf("Warning: failed to drop database '%s': %v", info.Database, err)
		}

		// Drop user
		dropUserSQL := fmt.Sprintf("DROP USER IF EXISTS %s;", info.Username)
		if _, err := m.adminDB.Exec(dropUserSQL); err != nil {
			log.Printf("Warning: failed to drop user '%s': %v", info.Username, err)
		}

		deleteSQL := `DELETE FROM javapaas_databases WHERE tenant_id = $1`
		_, _ = m.adminDB.Exec(deleteSQL, tenantID)
	}

	delete(m.store, tenantID)
	log.Printf("Deprovisioned database '%s' for tenant %s", info.Database, tenantID)
	return nil
}

// InjectDatabaseArgs automatically provisions a database if requested, and injects
// standard Spring Boot and Java system properties into the tenant's JVM args.
func (m *DBaaSManager) InjectDatabaseArgs(tenantID string, currentArgs []string) ([]string, *DatabaseInfo, error) {
	dbInfo, err := m.Provision(tenantID)
	if err != nil {
		return currentArgs, nil, err
	}

	injected := append([]string{}, currentArgs...)
	injected = append(injected,
		fmt.Sprintf("-Dspring.datasource.url=%s", dbInfo.JdbcURL),
		fmt.Sprintf("-Dspring.datasource.username=%s", dbInfo.Username),
		fmt.Sprintf("-Dspring.datasource.password=%s", dbInfo.Password),
		fmt.Sprintf("-Djavapaas.db.host=%s", dbInfo.Host),
		fmt.Sprintf("-Djavapaas.db.port=%d", dbInfo.Port),
		fmt.Sprintf("-Djavapaas.db.name=%s", dbInfo.Database),
	)

	return injected, dbInfo, nil
}
