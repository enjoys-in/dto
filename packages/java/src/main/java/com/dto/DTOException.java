package com.dto;

/**
 * Thrown when a schema is invalid or a transformation fails.
 */
public class DTOException extends RuntimeException {
    public DTOException(String message) {
        super("DTO transform error: " + message);
    }
}
