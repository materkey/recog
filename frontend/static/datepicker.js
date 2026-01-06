/**
 * Simple Date Picker Component
 * Adds date filtering functionality to the meetings list
 */

const DatePicker = {
    filterDate: localStorage.getItem('filterDate') || '',
    onChange: null,
    input: null,
    _initialized: false,

    init(inputId, onChange) {
        this.onChange = onChange;
        this.input = document.getElementById(inputId);
        if (!this.input) return;

        // Restore saved date
        this.input.value = this.filterDate;

        // Skip adding listeners if already initialized (prevents duplicates on view re-entry)
        if (this._initialized) return;
        this._initialized = true;

        // Handle change
        this.input.addEventListener('change', (e) => {
            this.setDate(e.target.value);
        });

        // Clear on double-click
        this.input.addEventListener('dblclick', () => {
            this.setDate('');
        });

        // Navigation buttons
        const prevBtn = document.getElementById('date-prev');
        const nextBtn = document.getElementById('date-next');

        if (prevBtn) {
            prevBtn.addEventListener('click', () => this.navigate(-1));
        }
        if (nextBtn) {
            nextBtn.addEventListener('click', () => this.navigate(1));
        }
    },

    setDate(dateStr) {
        this.filterDate = dateStr;
        this.input.value = dateStr;
        localStorage.setItem('filterDate', dateStr);
        if (this.onChange) this.onChange(dateStr);
    },

    navigate(days) {
        let date;
        if (this.filterDate) {
            // Parse as local midnight (not UTC) by adding time component
            date = new Date(this.filterDate + 'T00:00:00');
        } else {
            // Start from today at local midnight
            date = new Date();
            date.setHours(0, 0, 0, 0);
        }
        date.setDate(date.getDate() + days);
        // Format as local date (YYYY-MM-DD)
        const year = date.getFullYear();
        const month = String(date.getMonth() + 1).padStart(2, '0');
        const day = String(date.getDate()).padStart(2, '0');
        this.setDate(`${year}-${month}-${day}`);
    },

    getDate() {
        return this.filterDate;
    },

    // Filter meetings by date (YYYY-MM-DD format, local timezone)
    filterMeetings(meetings, dateStr) {
        if (!dateStr) return meetings;

        return meetings.filter(m => {
            const createdAt = m.created_at;
            if (!createdAt) return false;
            // Convert ISO timestamp to local date
            const date = new Date(createdAt);
            const year = date.getFullYear();
            const month = String(date.getMonth() + 1).padStart(2, '0');
            const day = String(date.getDate()).padStart(2, '0');
            const meetingDate = `${year}-${month}-${day}`;
            return meetingDate === dateStr;
        });
    }
};

// Export for use in app.js
window.DatePicker = DatePicker;
