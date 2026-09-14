package main

import (
	"database/sql"
	"encoding/json"
	"fmt"
	"log"
	"sync"
	"time"

	_ "github.com/lib/pq"
)

// PostgresAffinityStore implements the AffinityStore interface backed by PostgreSQL,
// enabling horizontal scaling and high-availability across multiple controller instances.
type PostgresAffinityStore struct {
	db *sql.DB
	mu sync.RWMutex
}

func NewPostgresAffinityStore(connStr string) (*PostgresAffinityStore, error) {
	db, err := sql.Open("postgres", connStr)
	if err != nil {
		return nil, fmt.Errorf("open postgres connection: %w", err)
	}

	db.SetMaxOpenConns(25)
	db.SetMaxIdleConns(5)
	db.SetConnMaxLifetime(5 * time.Minute)

	if err := db.Ping(); err != nil {
		db.Close()
		return nil, fmt.Errorf("ping postgres: %w", err)
	}

	store := &PostgresAffinityStore{db: db}
	if err := store.initSchema(); err != nil {
		db.Close()
		return nil, fmt.Errorf("init postgres schema: %w", err)
	}

	log.Println("Initialized PostgreSQL affinity store backend")
	return store, nil
}

func (s *PostgresAffinityStore) DB() *sql.DB {
	return s.db
}

func (s *PostgresAffinityStore) initSchema() error {
	query := `
	CREATE TABLE IF NOT EXISTS javapaas_tenants (
		tenant_id VARCHAR(128) PRIMARY KEY,
		node_id VARCHAR(128) NOT NULL,
		java_version VARCHAR(32) NOT NULL,
		tier VARCHAR(32) NOT NULL,
		jar_path TEXT NOT NULL,
		extra_args TEXT NOT NULL DEFAULT '[]',
		health_check_path VARCHAR(256) DEFAULT '',
		health_check_port INT DEFAULT 0,
		database_name VARCHAR(128) DEFAULT '',
		updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
	);`
	_, err := s.db.Exec(query)
	return err
}

func (s *PostgresAffinityStore) Get(tenantID string) (TenantSpec, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	query := `
	SELECT node_id, java_version, tier, jar_path, extra_args, health_check_path, health_check_port, database_name
	FROM javapaas_tenants
	WHERE tenant_id = $1`

	var spec TenantSpec
	var extraArgsJSON string
	err := s.db.QueryRow(query, tenantID).Scan(
		&spec.NodeID,
		&spec.JavaVersion,
		&spec.Tier,
		&spec.JarPath,
		&extraArgsJSON,
		&spec.HealthCheckPath,
		&spec.HealthCheckPort,
		&spec.Database,
	)
	if err != nil {
		if err != sql.ErrNoRows {
			log.Printf("Error querying tenant %s from postgres: %v", tenantID, err)
		}
		return TenantSpec{}, false
	}

	if extraArgsJSON != "" {
		_ = json.Unmarshal([]byte(extraArgsJSON), &spec.ExtraArgs)
	}
	return spec, true
}

func (s *PostgresAffinityStore) Set(tenantID string, spec TenantSpec) error {
	if err := spec.Validate(); err != nil {
		return err
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	extraArgsBytes, err := json.Marshal(spec.ExtraArgs)
	if err != nil {
		return fmt.Errorf("marshal extra_args: %w", err)
	}

	query := `
	INSERT INTO javapaas_tenants (
		tenant_id, node_id, java_version, tier, jar_path, extra_args, health_check_path, health_check_port, database_name, updated_at
	) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW())
	ON CONFLICT (tenant_id) DO UPDATE SET
		node_id = EXCLUDED.node_id,
		java_version = EXCLUDED.java_version,
		tier = EXCLUDED.tier,
		jar_path = EXCLUDED.jar_path,
		extra_args = EXCLUDED.extra_args,
		health_check_path = EXCLUDED.health_check_path,
		health_check_port = EXCLUDED.health_check_port,
		database_name = EXCLUDED.database_name,
		updated_at = NOW();`

	_, err = s.db.Exec(query,
		tenantID,
		spec.NodeID,
		spec.JavaVersion,
		spec.Tier,
		spec.JarPath,
		string(extraArgsBytes),
		spec.HealthCheckPath,
		spec.HealthCheckPort,
		spec.Database,
	)
	if err != nil {
		return fmt.Errorf("upsert tenant in postgres: %w", err)
	}
	return nil
}

func (s *PostgresAffinityStore) Delete(tenantID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	query := `DELETE FROM javapaas_tenants WHERE tenant_id = $1`
	_, err := s.db.Exec(query, tenantID)
	if err != nil {
		return fmt.Errorf("delete tenant from postgres: %w", err)
	}
	return nil
}

func (s *PostgresAffinityStore) GetAll() map[string]TenantSpec {
	s.mu.RLock()
	defer s.mu.RUnlock()

	result := make(map[string]TenantSpec)
	query := `
	SELECT tenant_id, node_id, java_version, tier, jar_path, extra_args, health_check_path, health_check_port, database_name
	FROM javapaas_tenants`

	rows, err := s.db.Query(query)
	if err != nil {
		log.Printf("Error querying all tenants from postgres: %v", err)
		return result
	}
	defer rows.Close()

	for rows.Next() {
		var tenantID string
		var spec TenantSpec
		var extraArgsJSON string

		err := rows.Scan(
			&tenantID,
			&spec.NodeID,
			&spec.JavaVersion,
			&spec.Tier,
			&spec.JarPath,
			&extraArgsJSON,
			&spec.HealthCheckPath,
			&spec.HealthCheckPort,
			&spec.Database,
		)
		if err != nil {
			log.Printf("Error scanning tenant row: %v", err)
			continue
		}

		if extraArgsJSON != "" {
			_ = json.Unmarshal([]byte(extraArgsJSON), &spec.ExtraArgs)
		}
		result[tenantID] = spec
	}

	return result
}

func (s *PostgresAffinityStore) Close() error {
	return s.db.Close()
}
